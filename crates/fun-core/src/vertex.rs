//! Google Cloud Vertex AI (Gemini generateContent stream).
//!
//! Same credential path as the coding agent: Application Default Credentials,
//! or a service-account JSON file. There is no Fun-side Vertex login.
//!
//! | Variable | Meaning |
//! | --- | --- |
//! | `GOOGLE_CLOUD_PROJECT` / `GCLOUD_PROJECT` | Project id. Config, then ADC `quota_project_id`, are fallbacks. |
//! | `GOOGLE_CLOUD_LOCATION` | Region, or `global`. Config is the fallback. Default `global` when both are unset. |
//! | `GOOGLE_APPLICATION_CREDENTIALS` | Service-account JSON. Else `~/.config/gcloud/application_default_credentials.json`. |
//! | `FUN_CODING_AGENT_MODEL` | Starting model. `vertex/…`, `google/…`, and a bare `gemini-*` id use Vertex. Default `grok-4.7`. |

use crate::provider::{CompleteReq, StreamSink};
use crate::session::{Call, Entry, Image, ToolResult};
use crate::tool::{Tool, aborted, is_abort};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

/// How long a stream may sit with no bytes before we give up. Thinking models
/// often pause well past a minute before the first SSE chunk.
const STREAM_IDLE: Duration = Duration::from_secs(10 * 60);
const TOKEN_TTL: Duration = Duration::from_secs(3000);
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const TOKEN_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
const ADC_REL: &str = ".config/gcloud/application_default_credentials.json";
const DEFAULT_MODEL: &str = "gemini-4-argon";

/// ADC or a service-account file is present. A token is minted on the first call.
pub fn logged_in() -> bool {
    adc_path().is_file()
}

fn env_trim(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn expand_tilde(path: &str) -> PathBuf {
    provider_grok::expand_tilde(path)
}

fn adc_path() -> PathBuf {
    if let Some(path) = env_trim("GOOGLE_APPLICATION_CREDENTIALS") {
        return expand_tilde(&path);
    }
    if let Ok(home) = env::var("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home).join(ADC_REL);
    }
    PathBuf::from(ADC_REL)
}

fn project_from_env() -> Option<String> {
    env_trim("GOOGLE_CLOUD_PROJECT").or_else(|| env_trim("GCLOUD_PROJECT"))
}

fn location_from_env() -> Option<String> {
    env_trim("GOOGLE_CLOUD_LOCATION")
}

fn project_id() -> Option<String> {
    project_from_env().or_else(|| crate::config::load().google_cloud_project)
}

fn location_id() -> Option<String> {
    location_from_env().or_else(|| crate::config::load().google_cloud_location)
}

#[derive(Clone, Debug)]
enum Credential {
    AccessToken {
        token: String,
        project: String,
        location: String,
    },
}

struct CachedToken {
    path: PathBuf,
    project: String,
    location: String,
    token: String,
    expires_at: Instant,
}

async fn resolve_credential(
    http: &reqwest::Client,
    cache: &Mutex<Option<CachedToken>>,
) -> Result<Credential> {
    let path = adc_path();
    if !path.is_file() {
        bail!(
            "Vertex AI uses Application Default Credentials. Run `gcloud auth application-default login`, then set GOOGLE_CLOUD_PROJECT and GOOGLE_CLOUD_LOCATION (or google_cloud_project and google_cloud_location in config.json). Or set GOOGLE_APPLICATION_CREDENTIALS to a service account key."
        );
    }
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let cred: CredFile = serde_json::from_str(&text)
        .with_context(|| format!("parse {}", path.display()))?;
    let project = project_id()
        .or_else(|| cred.quota_project_id.clone().filter(|s| !s.trim().is_empty()))
        .or_else(|| cred.project_id.clone().filter(|s| !s.trim().is_empty()))
        .with_context(|| {
            format!(
                "set GOOGLE_CLOUD_PROJECT or google_cloud_project in config.json (ADC file {} has no quota_project_id)",
                path.display()
            )
        })?;
    let location = location_id().unwrap_or_else(|| "global".into());
    let mut guard = cache.lock().await;
    if let Some(cached) = guard.as_ref()
        && cached.path == path
        && cached.project == project
        && cached.location == location
        && Instant::now() < cached.expires_at
    {
        return Ok(Credential::AccessToken {
            token: cached.token.clone(),
            project,
            location,
        });
    }
    let token = token_from_cred(http, &cred).await?;
    *guard = Some(CachedToken {
        path,
        project: project.clone(),
        location: location.clone(),
        token: token.clone(),
        expires_at: Instant::now() + TOKEN_TTL,
    });
    Ok(Credential::AccessToken {
        token,
        project,
        location,
    })
}

#[derive(Deserialize)]
struct CredFile {
    #[serde(rename = "type")]
    kind: Option<String>,
    client_email: Option<String>,
    private_key: Option<String>,
    token_uri: Option<String>,
    // authorized-user ADC
    client_id: Option<String>,
    client_secret: Option<String>,
    refresh_token: Option<String>,
    quota_project_id: Option<String>,
    project_id: Option<String>,
}

async fn token_from_cred(http: &reqwest::Client, cred: &CredFile) -> Result<String> {
    let kind = cred.kind.as_deref().unwrap_or("");
    if kind == "service_account" || cred.private_key.is_some() {
        return service_account_token(http, cred).await;
    }
    if kind == "authorized_user" || cred.refresh_token.is_some() {
        return refresh_user_token(http, cred).await;
    }
    bail!("unsupported credentials (expected service_account or authorized_user)")
}

async fn refresh_user_token(http: &reqwest::Client, cred: &CredFile) -> Result<String> {
    let client_id = cred
        .client_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .context("ADC file missing client_id")?;
    let client_secret = cred
        .client_secret
        .as_deref()
        .filter(|s| !s.is_empty())
        .context("ADC file missing client_secret")?;
    let refresh = cred
        .refresh_token
        .as_deref()
        .filter(|s| !s.is_empty())
        .context("ADC file missing refresh_token")?;
    let body = format!(
        "grant_type=refresh_token&client_id={}&client_secret={}&refresh_token={}",
        urlencoding_encode(client_id),
        urlencoding_encode(client_secret),
        urlencoding_encode(refresh)
    );
    post_token(http, TOKEN_URL, &body).await
}

async fn service_account_token(http: &reqwest::Client, cred: &CredFile) -> Result<String> {
    let email = cred
        .client_email
        .as_deref()
        .filter(|s| !s.is_empty())
        .context("service account missing client_email")?;
    let key = cred
        .private_key
        .as_deref()
        .filter(|s| !s.is_empty())
        .context("service account missing private_key")?;
    let token_uri = cred
        .token_uri
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(TOKEN_URL);
    if !https_google_url(token_uri) {
        bail!("refusing service-account token_uri (must be https on googleapis.com)");
    }
    let assertion = sign_jwt(email, token_uri, key)?;
    let body = format!(
        "grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer&assertion={}",
        urlencoding_encode(&assertion)
    );
    post_token(http, token_uri, &body).await
}

fn https_google_url(raw: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(raw) else {
        return false;
    };
    // No userinfo (`https://x@host`), no non-default port. Host must be
    // googleapis.com itself, not a suffix trick and not an empty label.
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    if url.port().is_some() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("googleapis.com") {
        return true;
    }
    let Some(prefix) = host.strip_suffix(".googleapis.com") else {
        return false;
    };
    !prefix.is_empty()
        && !prefix.contains('.')
        && prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

async fn post_token(http: &reqwest::Client, url: &str, body: &str) -> Result<String> {
    let resp = http
        .post(url)
        .timeout(Duration::from_secs(30))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(body.to_string())
        .send()
        .await
        .context("token request")?;
    let status = resp.status().as_u16();
    let text = resp.text().await.context("token response body")?;
    if !(200..300).contains(&status) {
        // Body is parsed only on success. A failure must not echo it:
        // token endpoints sometimes repeat the assertion.
        bail!("token endpoint {status}");
    }
    let v: Value = serde_json::from_str(&text).context("token json")?;
    v.get("access_token")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .context("token response missing access_token")
}

fn urlencoding_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn b64url(bytes: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n = (u32::from(bytes[i]) << 16) | (u32::from(bytes[i + 1]) << 8) | u32::from(bytes[i + 2]);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(T[((n >> 6) & 63) as usize] as char);
        out.push(T[(n & 63) as usize] as char);
        i += 3;
    }
    match bytes.len() - i {
        1 => {
            let n = u32::from(bytes[i]) << 16;
            out.push(T[((n >> 18) & 63) as usize] as char);
            out.push(T[((n >> 12) & 63) as usize] as char);
        }
        2 => {
            let n = (u32::from(bytes[i]) << 16) | (u32::from(bytes[i + 1]) << 8);
            out.push(T[((n >> 18) & 63) as usize] as char);
            out.push(T[((n >> 12) & 63) as usize] as char);
            out.push(T[((n >> 6) & 63) as usize] as char);
        }
        _ => {}
    }
    out
}

fn sign_jwt(email: &str, aud: &str, pem: &str) -> Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let header = b64url(br#"{"alg":"RS256","typ":"JWT"}"#);
    let claims = json!({
        "iss": email,
        "scope": TOKEN_SCOPE,
        "aud": aud,
        "iat": now,
        "exp": now + 3600,
    });
    let payload = b64url(serde_json::to_vec(&claims)?.as_slice());
    let signing_input = format!("{header}.{payload}");
    let sig = rsa_sha256(pem, signing_input.as_bytes())?;
    Ok(format!("{signing_input}.{}", b64url(&sig)))
}

fn rsa_sha256(pem: &str, message: &[u8]) -> Result<Vec<u8>> {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(message);
    let der = pem_body(pem)?;
    let (n, d) = rsa_private(&der)?;
    let k = n.len();
    if k < 64 {
        bail!("rsa modulus too small");
    }
    let em = emsa_pkcs1_sha256(&digest, k)?;
    let sig = mod_exp(&em, &d, &n)?;
    if sig.len() > k {
        bail!("rsa signature longer than modulus");
    }
    let mut out = vec![0u8; k - sig.len()];
    out.extend(sig);
    Ok(out)
}

fn emsa_pkcs1_sha256(digest: &[u8], k: usize) -> Result<Vec<u8>> {
    // DigestInfo for SHA-256
    const PREFIX: &[u8] = &[
        0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01,
        0x05, 0x00, 0x04, 0x20,
    ];
    let mut t = PREFIX.to_vec();
    t.extend_from_slice(digest);
    if k < t.len() + 11 {
        bail!("rsa modulus too small for SHA-256 DigestInfo");
    }
    let mut em = vec![0xffu8; k];
    em[0] = 0x00;
    em[1] = 0x01;
    em[k - t.len() - 1] = 0x00;
    em[k - t.len()..].copy_from_slice(&t);
    Ok(em)
}

fn pem_body(pem: &str) -> Result<Vec<u8>> {
    let mut b64 = String::new();
    for line in pem.lines() {
        let line = line.trim();
        if line.starts_with("-----") || line.is_empty() {
            continue;
        }
        b64.push_str(line);
    }
    b64_decode(&b64).context("decode PEM")
}

fn b64_decode(s: &str) -> Result<Vec<u8>> {
    fn val(c: u8) -> Result<u8> {
        match c {
            b'A'..=b'Z' => Ok(c - b'A'),
            b'a'..=b'z' => Ok(c - b'a' + 26),
            b'0'..=b'9' => Ok(c - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => bail!("bad base64"),
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let mut chunk = [b'='; 4];
        let mut n = 0;
        while n < 4 && i < bytes.len() {
            chunk[n] = bytes[i];
            n += 1;
            i += 1;
        }
        if n == 0 {
            break;
        }
        if n < 4 {
            bail!("bad base64 length");
        }
        let a = val(chunk[0])?;
        let b = val(chunk[1])?;
        out.push((a << 2) | (b >> 4));
        if chunk[2] != b'=' {
            let c = val(chunk[2])?;
            out.push((b << 4) | (c >> 2));
            if chunk[3] != b'=' {
                let d = val(chunk[3])?;
                out.push((c << 6) | d);
            }
        }
    }
    Ok(out)
}

struct Der<'a> {
    data: &'a [u8],
    i: usize,
}

impl<'a> Der<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, i: 0 }
    }
    fn read_tlv(&mut self) -> Result<(u8, &'a [u8])> {
        if self.i >= self.data.len() {
            bail!("truncated der");
        }
        let tag = self.data[self.i];
        self.i += 1;
        if self.i >= self.data.len() {
            bail!("truncated der length");
        }
        let mut len = self.data[self.i] as usize;
        self.i += 1;
        if len & 0x80 != 0 {
            let n = len & 0x7f;
            if n > 4 || self.i + n > self.data.len() {
                bail!("bad der length");
            }
            len = 0;
            for _ in 0..n {
                len = (len << 8) | self.data[self.i] as usize;
                self.i += 1;
            }
        }
        if self.i + len > self.data.len() {
            bail!("truncated der value");
        }
        let v = &self.data[self.i..self.i + len];
        self.i += len;
        Ok((tag, v))
    }
    fn integer(&mut self) -> Result<&'a [u8]> {
        let (tag, v) = self.read_tlv()?;
        if tag != 0x02 {
            bail!("expected integer");
        }
        Ok(v)
    }
    fn seq(&mut self) -> Result<Der<'a>> {
        let (tag, v) = self.read_tlv()?;
        if tag != 0x30 {
            bail!("expected sequence");
        }
        Ok(Der::new(v))
    }
}

fn rsa_private(der: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    pkcs8_private_key(der).or_else(|_| pkcs1_private_key(der))
}

fn pkcs8_private_key(der: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut d = Der::new(der);
    let mut seq = d.seq()?;
    let _version = seq.integer()?;
    let _alg = seq.seq()?;
    let (tag, oct) = seq.read_tlv()?;
    if tag != 0x04 {
        bail!("expected octet string");
    }
    pkcs1_private_key(oct)
}

fn pkcs1_private_key(der: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut d = Der::new(der);
    let mut seq = d.seq()?;
    let _version = seq.integer()?;
    let n = trim_be(seq.integer()?);
    let _e = seq.integer()?;
    let exp = trim_be(seq.integer()?);
    Ok((n, exp))
}

fn trim_be(bytes: &[u8]) -> Vec<u8> {
    let i = bytes.iter().position(|b| *b != 0).unwrap_or(0);
    let mut out = bytes[i..].to_vec();
    if out.is_empty() {
        out.push(0);
    }
    out
}

/// `base^exp mod m` on 64-bit little-endian limbs. Inputs and output are big-endian bytes.
fn mod_exp(base: &[u8], exp: &[u8], modulus: &[u8]) -> Result<Vec<u8>> {
    if modulus.iter().all(|b| *b == 0) {
        bail!("rsa modulus is zero");
    }
    let m = be_to_limbs(modulus);
    let mut result = vec![1u64];
    let mut b = rem_limbs(&be_to_limbs(base), &m);
    for &byte in exp.iter().rev() {
        for bit in 0..8 {
            if (byte >> bit) & 1 == 1 {
                result = mul_mod_limbs(&result, &b, &m);
            }
            b = mul_mod_limbs(&b, &b, &m);
        }
    }
    Ok(limbs_to_be(&result))
}

fn be_to_limbs(bytes: &[u8]) -> Vec<u64> {
    let bytes = trim_be(bytes);
    let mut limbs = Vec::with_capacity(bytes.len().div_ceil(8));
    let mut i = bytes.len();
    while i > 0 {
        let start = i.saturating_sub(8);
        let mut limb = 0u64;
        for &b in &bytes[start..i] {
            limb = (limb << 8) | u64::from(b);
        }
        limbs.push(limb);
        i = start;
    }
    trim_limbs(&mut limbs);
    limbs
}

fn limbs_to_be(limbs: &[u64]) -> Vec<u8> {
    let mut out = Vec::with_capacity(limbs.len() * 8);
    for &limb in limbs.iter().rev() {
        out.extend_from_slice(&limb.to_be_bytes());
    }
    trim_be(&out)
}

fn trim_limbs(limbs: &mut Vec<u64>) {
    while limbs.last() == Some(&0) && limbs.len() > 1 {
        limbs.pop();
    }
    if limbs.is_empty() {
        limbs.push(0);
    }
}

fn mul_mod_limbs(a: &[u64], b: &[u64], m: &[u64]) -> Vec<u64> {
    let mut prod = vec![0u64; a.len() + b.len()];
    for (i, &av) in a.iter().enumerate() {
        if av == 0 {
            continue;
        }
        let mut carry = 0u128;
        for (j, &bv) in b.iter().enumerate() {
            let cur = u128::from(prod[i + j]) + u128::from(av) * u128::from(bv) + carry;
            prod[i + j] = cur as u64;
            carry = cur >> 64;
        }
        prod[i + b.len()] = carry as u64;
    }
    trim_limbs(&mut prod);
    rem_limbs(&prod, m)
}

fn rem_limbs(a: &[u64], m: &[u64]) -> Vec<u64> {
    if cmp_limbs(a, m).is_lt() {
        let mut out = a.to_vec();
        trim_limbs(&mut out);
        return out;
    }
    let n = m.len();
    if n == 1 {
        let d = u128::from(m[0]);
        let mut rem = 0u128;
        for &limb in a.iter().rev() {
            rem = ((rem << 64) | u128::from(limb)) % d;
        }
        return vec![rem as u64];
    }
    let shift = m[n - 1].leading_zeros();
    let m_norm = shl_limbs(m, shift);
    let mut u = shl_limbs(a, shift);
    if u.len() <= n {
        u.resize(n + 1, 0);
    } else {
        u.push(0);
    }
    let m_top = m_norm[n - 1];
    for j in (0..=u.len() - 1 - n).rev() {
        if u[j + n] >= m_top {
            sub_mul_limb(&mut u[j + 1..=j + n], &m_norm, 1);
        }
        let top = (u128::from(u[j + n]) << 64) | u128::from(u[j + n - 1]);
        let q = if m_top == u64::MAX {
            u[j + n]
        } else {
            (top / (u128::from(m_top) + 1)).min(u128::from(u64::MAX)) as u64
        };
        if q > 0 {
            sub_mul_limb(&mut u[j..=j + n], &m_norm, q);
        }
        while !cmp_limbs(&u[j..=j + n], &m_norm).is_lt() {
            sub_mul_limb(&mut u[j..=j + n], &m_norm, 1);
        }
    }
    let mut rem = shr_limbs(&u[..n], shift);
    trim_limbs(&mut rem);
    rem
}

fn shl_limbs(a: &[u64], shift: u32) -> Vec<u64> {
    if shift == 0 {
        return a.to_vec();
    }
    let mut out = Vec::with_capacity(a.len() + 1);
    let mut carry = 0u64;
    for &limb in a {
        out.push((limb << shift) | carry);
        carry = limb >> (64 - shift);
    }
    if carry != 0 {
        out.push(carry);
    }
    out
}

fn shr_limbs(a: &[u64], shift: u32) -> Vec<u64> {
    if shift == 0 {
        return a.to_vec();
    }
    let mut out = vec![0u64; a.len()];
    let mut carry = 0u64;
    for (i, &limb) in a.iter().enumerate().rev() {
        out[i] = (limb >> shift) | carry;
        carry = limb << (64 - shift);
    }
    out
}

fn sub_mul_limb(u: &mut [u64], m: &[u64], q: u64) {
    let mut borrow = 0u128;
    for (i, slot) in u.iter_mut().enumerate() {
        let mv = m.get(i).copied().unwrap_or(0);
        let prod = u128::from(mv) * u128::from(q) + borrow;
        let lo = prod as u64;
        let hi = prod >> 64;
        let (diff, under) = slot.overflowing_sub(lo);
        *slot = diff;
        borrow = hi + u128::from(under);
    }
}

fn cmp_limbs(a: &[u64], b: &[u64]) -> std::cmp::Ordering {
    let alen = a.iter().rposition(|&x| x != 0).map_or(0, |i| i + 1);
    let blen = b.iter().rposition(|&x| x != 0).map_or(0, |i| i + 1);
    alen.cmp(&blen)
        .then_with(|| a[..alen].iter().rev().cmp(b[..blen].iter().rev()))
}

pub fn default_model() -> String {
    env_trim("FUN_CODING_AGENT_MODEL").unwrap_or_else(|| DEFAULT_MODEL.into())
}

pub fn thinking_level(effort: &str) -> &'static str {
    match effort {
        "low" | "minimal" => "LOW",
        "high" | "xhigh" => "HIGH",
        _ => "MEDIUM",
    }
}

/// Gemini 3 and Gemini 4 use a thinking level. Gemini 2.5 uses a token budget.
/// Gemini 4 Argon rejects `thinkingBudget` and `thinkingLevel=MINIMAL`.
pub fn uses_thinking_level(model: &str) -> bool {
    let id = model.to_ascii_lowercase();
    id.contains("gemini-4")
        || id.contains("argon")
        || id.contains("gemini-3")
        || id.contains("-3-")
        || id.contains("3.0")
        || id.contains("3.1")
}

fn is_argon(model: &str) -> bool {
    let id = model.to_ascii_lowercase();
    id.contains("gemini-4") || id.contains("argon")
}

fn thinking_budget(model: &str, effort: &str) -> i64 {
    let id = model.to_ascii_lowercase();
    let table: [i64; 4] = if id.contains("2.5-flash") {
        [128, 2048, 8192, 24576]
    } else if id.contains("2.5-pro") {
        [128, 2048, 8192, 32768]
    } else {
        return -1;
    };
    match effort {
        "minimal" => table[0],
        "low" => table[1],
        "high" | "xhigh" => table[3],
        _ => table[2],
    }
}

fn host(location: &str) -> String {
    if location.eq_ignore_ascii_case("global") {
        "https://aiplatform.googleapis.com".into()
    } else {
        format!("https://{location}-aiplatform.googleapis.com")
    }
}

fn endpoint(cred: &Credential, model: &str) -> String {
    let Credential::AccessToken {
        project, location, ..
    } = cred;
    format!(
        "{}/v1/projects/{}/locations/{}/publishers/google/models/{}:streamGenerateContent?alt=sse",
        host(location),
        project,
        location,
        model
    )
}

fn tool_decl(tool: &Tool) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for f in tool.properties {
        properties.insert(
            f.name.into(),
            json!({ "type": schema_type(f.r#type), "description": f.description }),
        );
        if f.required {
            required.push(f.name);
        }
    }
    json!({
        "name": tool.name,
        "description": tool.description,
        "parameters": {
            "type": "OBJECT",
            "properties": properties,
            "required": required,
        }
    })
}

fn schema_type(t: &str) -> &'static str {
    match t {
        "integer" | "number" => "NUMBER",
        "boolean" => "BOOLEAN",
        "array" => "ARRAY",
        _ => "STRING",
    }
}

fn parts_for_user(text: &str, images: &[Image]) -> Vec<Value> {
    let mut parts = Vec::new();
    if !text.is_empty() {
        parts.push(json!({ "text": text }));
    }
    for image in images {
        if image.kind() == "text"
            && image.media == "txt"
            && let Some(plain) = image.decode_utf8()
        {
            let name = if image.name.is_empty() {
                "file.txt"
            } else {
                image.name.as_str()
            };
            parts.push(json!({ "text": format!("--- {name} ---\n{plain}") }));
            continue;
        }
        parts.push(json!({
            "inlineData": {
                "mimeType": image.media_type(),
                "data": image.data,
            }
        }));
    }
    if parts.is_empty() {
        parts.push(json!({ "text": "" }));
    }
    parts
}

/// Path-only reminder for a successful stub. Not a function call, and not a
/// claim that the file still matches what was written.
fn stub_path_note(calls: &[&Call]) -> String {
    let labels: Vec<String> = calls
        .iter()
        .map(|c| {
            c.args
                .get("path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(|p| format!("{} {p}", c.name))
                .unwrap_or_else(|| c.name.clone())
        })
        .collect();
    if labels.is_empty() {
        return String::new();
    }
    format!(
        "(earlier {} compacted from context — path only; read the file before repeating or deleting.)",
        labels.join(", ")
    )
}

fn function_response_part(t: &ToolResult) -> Value {
    let response = if t.is_error {
        json!({ "error": t.content })
    } else {
        json!({ "output": t.content })
    };
    json!({
        "functionResponse": {
            "name": t.name,
            "response": response,
        }
    })
}

fn push_user(out: &mut Vec<Value>, text: &str, images: &[Image]) {
    out.push(json!({
        "role": "user",
        "parts": parts_for_user(text, images),
    }));
}

/// Prune puts latest write/edit paths back as compact stubs (`restored`, no body).
/// Those are history notes, not calls the model made. Replaying them as
/// functionCall makes Gemini echo the same incomplete Write on the next turn
/// (`write args: missing field content`).
fn is_restore_stub(c: &Call) -> bool {
    c.restored
}

fn push_model(out: &mut Vec<Value>, text: &str, calls: &[Call], following: &[Entry]) {
    let mut parts = Vec::new();
    let text = if calls.is_empty() {
        replay_text(text)
    } else {
        text.to_string()
    };
    if !text.is_empty() {
        parts.push(json!({ "text": text }));
    }
    let (stubs, live): (Vec<&Call>, Vec<&Call>) = calls.iter().partition(|c| is_restore_stub(c));
    if !stubs.is_empty() {
        let results = tool_results(following);
        let mut note = Call::restored_note(&stubs, &results);
        if note.is_empty() {
            note = stub_path_note(&stubs);
        }
        if !note.is_empty() {
            parts.push(json!({ "text": note }));
        }
    }
    for c in live {
        let mut function_call = serde_json::Map::new();
        function_call.insert("name".into(), json!(c.name));
        function_call.insert("args".into(), c.args.clone());
        let mut part = serde_json::Map::new();
        part.insert("functionCall".into(), Value::Object(function_call));
        if !c.thought_signature.is_empty() {
            part.insert("thoughtSignature".into(), json!(c.thought_signature));
        }
        parts.push(Value::Object(part));
    }
    if parts.is_empty() {
        return;
    }
    out.push(json!({ "role": "model", "parts": parts }));
}

/// Same clip as the Grok wire: a long tool-less plan must not be replayed in full.
fn replay_text(text: &str) -> String {
    const MAX: usize = 1200;
    const HEAD: usize = 700;
    const TAIL: usize = 400;
    if text.len() <= MAX {
        return text.to_string();
    }
    let head = crate::tool::clip_utf8(text, HEAD);
    let mut tail_at = text.len().saturating_sub(TAIL);
    while tail_at < text.len() && !text.is_char_boundary(tail_at) {
        tail_at += 1;
    }
    format!("{head}\n…(monologue clipped)…\n{}", &text[tail_at..])
}

fn tool_results(following: &[Entry]) -> Vec<ToolResult> {
    let mut out = Vec::new();
    for e in following {
        match e {
            Entry::Tool(t) => out.push(t.clone()),
            Entry::User { .. } | Entry::Assistant { .. } => break,
        }
    }
    out
}

fn push_tool_turn(out: &mut Vec<Value>, tools: &[&ToolResult]) {
    if tools.is_empty() {
        return;
    }
    let parts: Vec<Value> = tools.iter().copied().map(function_response_part).collect();
    out.push(json!({ "role": "user", "parts": parts }));
}

/// Gemini requires one user turn of functionResponse parts, same count and
/// order as the functionCall parts in the preceding model turn.
fn contents(entries: &[Entry]) -> Vec<Value> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < entries.len() {
        match &entries[i] {
            Entry::User { text, images } => {
                push_user(&mut out, text, images);
                i += 1;
            }
            Entry::Assistant { text, calls, .. } => {
                push_model(&mut out, text, calls, &entries[i + 1..]);
                let expected = calls.iter().filter(|c| !is_restore_stub(c)).count();
                i += 1;
                if expected == 0 {
                    while i < entries.len()
                        && matches!(&entries[i], Entry::Tool(t) if t.restored)
                    {
                        i += 1;
                    }
                    continue;
                }
                let mut tools = Vec::new();
                while tools.len() < expected && i < entries.len() {
                    match &entries[i] {
                        Entry::Tool(t) if t.restored => i += 1,
                        Entry::Tool(t) => {
                            tools.push(t);
                            i += 1;
                        }
                        Entry::User { text, images } => {
                            push_tool_turn(&mut out, &tools);
                            tools.clear();
                            push_user(&mut out, text, images);
                            i += 1;
                            break;
                        }
                        Entry::Assistant { .. } => break,
                    }
                }
                push_tool_turn(&mut out, &tools);
            }
            Entry::Tool(t) if t.restored => i += 1,
            Entry::Tool(t) => {
                push_tool_turn(&mut out, &[t]);
                i += 1;
            }
        }
    }
    out
}

pub fn build_body(model: &str, system: &str, entries: &[Entry], tools: &[Tool], effort: &str) -> Value {
    let mut body = Map::new();
    body.insert("contents".into(), Value::Array(contents(entries)));
    if !system.is_empty() {
        body.insert(
            "systemInstruction".into(),
            json!({ "parts": [{ "text": system }] }),
        );
    }
    if !tools.is_empty() {
        let decls: Vec<Value> = tools.iter().map(tool_decl).collect();
        body.insert("tools".into(), json!([{ "functionDeclarations": decls }]));
    }
    let mut thinking = Map::new();
    if uses_thinking_level(model) {
        thinking.insert("includeThoughts".into(), json!(true));
        thinking.insert("thinkingLevel".into(), json!(thinking_level(effort)));
    } else {
        let budget = thinking_budget(model, effort);
        thinking.insert("thinkingBudget".into(), json!(budget));
        thinking.insert("includeThoughts".into(), json!(budget != 0));
    }
    let mut generation = Map::new();
    generation.insert("thinkingConfig".into(), Value::Object(thinking));
    // Argon caps one streaming GenerateContent reply at 65,536 output tokens.
    if is_argon(model) {
        generation.insert("maxOutputTokens".into(), json!(65536));
    }
    body.insert("generationConfig".into(), Value::Object(generation));
    Value::Object(body)
}

#[derive(Clone)]
pub struct Vertex {
    http: reqwest::Client,
    token_cache: Arc<Mutex<Option<CachedToken>>>,
    pub effort: String,
}

impl Vertex {
    pub fn client(effort: String) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(15))
                .tcp_nodelay(true)
                .tcp_keepalive(Duration::from_secs(20))
                .pool_idle_timeout(Duration::from_secs(90))
                // Reqwest's default 30s request timeout covers the whole call,
                // including idle gaps between SSE chunks. Thinking models often
                // sit silent for minutes. Idle gaps are bounded by read_timeout
                // (STREAM_IDLE); the total deadline only caps a runaway call.
                .timeout(Duration::from_secs(60 * 60))
                .read_timeout(STREAM_IDLE)
                .build()
                .context("vertex http client")?,
            token_cache: Arc::new(Mutex::new(None)),
            effort,
        })
    }

    pub async fn from_env(effort: String) -> Result<(Self, String)> {
        let client = Self::client(effort)?;
        // Fail fast if nothing can authenticate.
        let _ = resolve_credential(&client.http, &client.token_cache).await?;
        Ok((client, default_model()))
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub async fn stream_url(
        &self,
        url: &str,
        body: &Value,
        label: &str,
        bearer: Option<&str>,
        api_key: Option<&str>,
        req: &CompleteReq<'_>,
        mut on_delta: impl FnMut(&str),
        mut on_think: impl FnMut(&str),
    ) -> Result<crate::grok::Reply> {
        let mut delay = Duration::from_millis(250);
        let mut started = false;
        let mut attempt = 0u32;
        loop {
            let mut sink = StreamSink {
                started: &mut started,
                on_delta: &mut on_delta,
                on_think: &mut on_think,
            };
            match stream_at(&self.http, url, body, label, bearer, api_key, req, &mut sink).await {
                Ok(r) => return Ok(r),
                Err(e) if is_abort(&e) => return Err(e),
                Err(e) => {
                    attempt += 1;
                    if attempt >= 3 || started || !is_transient(&e) {
                        return Err(e);
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = req.abort.wait() => return Err(aborted()),
                    }
                    delay *= 2;
                }
            }
        }
    }

    pub async fn complete(
        &self,
        req: CompleteReq<'_>,
        on_delta: impl FnMut(&str),
        on_think: impl FnMut(&str),
    ) -> Result<crate::grok::Reply> {
        self.complete_with_effort(req, &self.effort.clone(), on_delta, on_think)
            .await
    }

    pub async fn complete_with_effort(
        &self,
        req: CompleteReq<'_>,
        effort: &str,
        on_delta: impl FnMut(&str),
        on_think: impl FnMut(&str),
    ) -> Result<crate::grok::Reply> {
        let cred = resolve_credential(&self.http, &self.token_cache).await?;
        let url = endpoint(&cred, req.model);
        let body = build_body(req.model, req.system, req.entries, req.tools, effort);
        let token = match &cred {
            Credential::AccessToken { token, .. } => token.clone(),
        };
        self.stream_url(&url, &body, "vertex", Some(&token), None, &req, on_delta, on_think)
            .await
    }
}

async fn stream_at(
    http: &reqwest::Client,
    url: &str,
    body: &Value,
    label: &str,
    bearer: Option<&str>,
    api_key: Option<&str>,
    req: &CompleteReq<'_>,
    sink: &mut StreamSink<'_>,
) -> Result<crate::grok::Reply> {
    let mut builder = http
        .post(url)
        .header("Accept", "text/event-stream")
        .header("Content-Type", "application/json");
    if let Some(token) = bearer {
        builder = builder.bearer_auth(token);
    }
    if let Some(key) = api_key {
        builder = builder.header("x-goog-api-key", key);
    }
    let send = builder.json(body).send();
    let mut resp = tokio::select! {
        r = send => r.with_context(|| format!("{label} request"))?,
        _ = req.abort.wait() => return Err(aborted()),
    };
    let status = resp.status();
    if !status.is_success() {
        // Body can contain request echoes or tokens. Drain it so the
        // connection can be reused, but do not put it in the error.
        let _ = resp.text().await;
        bail!("{label} {status}");
    }
    let mut buf = String::new();
    let mut text = String::new();
    let mut thinking = String::new();
    let mut calls: Vec<Call> = Vec::new();
    let mut finish = String::new();
    let mut input_tokens = 0u64;
    let mut output_tokens = 0u64;
    let mut cached_tokens = 0u64;
    let mut reasoning_tokens = 0u64;
    let mut call_n = 0u64;
    let mut pending_sig = String::new();
    let started = {
        let mut acc = ChunkAcc {
            text: &mut text,
            thinking: &mut thinking,
            calls: &mut calls,
            finish: &mut finish,
            input_tokens: &mut input_tokens,
            output_tokens: &mut output_tokens,
            cached_tokens: &mut cached_tokens,
            reasoning_tokens: &mut reasoning_tokens,
            call_n: &mut call_n,
            pending_sig: &mut pending_sig,
            sink,
        };
        let mut apply = |data: &str| -> Result<()> {
            for obj in split_json_objects(data) {
                if obj.is_empty() || obj == "[DONE]" {
                    continue;
                }
                apply_chunk(obj, &mut acc)?;
            }
            Ok(())
        };
        loop {
            let chunk = tokio::select! {
                c = resp.chunk() => c,
                _ = req.abort.wait() => return Err(aborted()),
            };
            let Some(chunk) = chunk.with_context(|| format!("{label} stream"))? else {
                break;
            };
            buf.push_str(&String::from_utf8_lossy(&chunk));
            // streamGenerateContent?alt=sse is newline-delimited JSON
            // (one object per line), not SSE blocks separated by blank lines.
            while let Some(idx) = buf.find('\n') {
                let line = buf[..idx].trim().trim_end_matches('\r').to_string();
                buf.drain(..idx + 1);
                if line.is_empty() {
                    continue;
                }
                apply(&line_payload(&line))?;
            }
        }
        if !buf.trim().is_empty() {
            apply(&line_payload(buf.trim()))?;
        }
        *acc.sink.started
    };
    if finish.is_empty() && text.is_empty() && calls.is_empty() && !started {
        bail!("{label} stream ended without a finish reason");
    }
    let truncated = finish.eq_ignore_ascii_case("MAX_TOKENS");
    Ok(crate::grok::Reply {
        text,
        thinking,
        calls,
        truncated,
        input_tokens,
        output_tokens,
        cached_tokens,
        reasoning_tokens,
    })
}

fn line_payload(line: &str) -> String {
    line.trim()
        .strip_prefix("data:")
        .map(str::trim_start)
        .unwrap_or(line.trim())
        .to_string()
}

fn split_json_objects(data: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = data.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        if bytes[i] != b'{' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            let piece = data[start..i].trim();
            if !piece.is_empty() {
                out.push(piece);
            }
            continue;
        }
        let start = i;
        let mut depth = 0i32;
        let mut in_str = false;
        let mut escape = false;
        while i < bytes.len() {
            let b = bytes[i];
            if in_str {
                if escape {
                    escape = false;
                } else if b == b'\\' {
                    escape = true;
                } else if b == b'"' {
                    in_str = false;
                }
            } else if b == b'"' {
                in_str = true;
            } else if b == b'{' {
                depth += 1;
            } else if b == b'}' {
                depth -= 1;
                if depth == 0 {
                    i += 1;
                    out.push(data[start..i].trim());
                    break;
                }
            }
            i += 1;
        }
        if depth != 0 {
            // Incomplete object; caller only passes complete lines.
            break;
        }
    }
    out
}

struct ChunkAcc<'sink, 'acc> {
    text: &'acc mut String,
    thinking: &'acc mut String,
    calls: &'acc mut Vec<Call>,
    finish: &'acc mut String,
    input_tokens: &'acc mut u64,
    output_tokens: &'acc mut u64,
    cached_tokens: &'acc mut u64,
    reasoning_tokens: &'acc mut u64,
    call_n: &'acc mut u64,
    pending_sig: &'acc mut String,
    sink: &'acc mut StreamSink<'sink>,
}

fn apply_chunk(data: &str, acc: &mut ChunkAcc<'_, '_>) -> Result<()> {
    let v: Value = serde_json::from_str(data).with_context(|| format!("vertex chunk: {data}"))?;
    // generateContent may wrap the payload in `response`. Either shape is fine.
    let v = v.get("response").cloned().unwrap_or(v);
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("vertex error");
        bail!("vertex error: {msg}");
    }
    if let Some(cands) = v.get("candidates").and_then(|c| c.as_array())
        && let Some(cand) = cands.first()
    {
        if let Some(reason) = cand.get("finishReason").and_then(|r| r.as_str()) {
            *acc.finish = reason.to_string();
        }
        if let Some(parts) = cand
            .pointer("/content/parts")
            .and_then(|p| p.as_array())
        {
            for part in parts {
                let sig = part
                    .get("thoughtSignature")
                    .and_then(|s| s.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let is_thought = part.get("thought").and_then(|t| t.as_bool()).unwrap_or(false);
                if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                    *acc.sink.started = true;
                    if is_thought {
                        acc.thinking.push_str(t);
                        (acc.sink.on_think)(t);
                    } else if !t.is_empty() {
                        acc.text.push_str(t);
                        (acc.sink.on_delta)(t);
                    }
                }
                if let Some(fc) = part.get("functionCall") {
                    *acc.sink.started = true;
                    let name = fc.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                    let args = fc.get("args").cloned().unwrap_or_else(|| json!({}));
                    *acc.call_n += 1;
                    let id = fc
                        .get("id")
                        .and_then(|i| i.as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("{name}_{}", acc.call_n));
                    let thought_signature = sig
                        .clone()
                        .or_else(|| {
                            fc.get("thoughtSignature")
                                .and_then(|s| s.as_str())
                                .filter(|s| !s.is_empty())
                                .map(str::to_string)
                        })
                        .or_else(|| (!acc.pending_sig.is_empty()).then(|| std::mem::take(acc.pending_sig)))
                        .unwrap_or_default();
                    if !thought_signature.is_empty() {
                        acc.pending_sig.clear();
                    }
                    acc.calls.push(Call {
                        id,
                        name,
                        args,
                        thought_signature,
                        restored: false,
                    });
                } else if let Some(sig) = sig {
                    if let Some(last) = acc.calls.last_mut().filter(|c| c.thought_signature.is_empty()) {
                        last.thought_signature = sig;
                    } else {
                        *acc.pending_sig = sig;
                    }
                }
            }
        }
    }
    if let Some(u) = v.get("usageMetadata") {
        let prompt = u.get("promptTokenCount").and_then(|n| n.as_u64()).unwrap_or(0);
        let cached = u
            .get("cachedContentTokenCount")
            .and_then(|n| n.as_u64())
            .unwrap_or(0);
        let candidates = u
            .get("candidatesTokenCount")
            .and_then(|n| n.as_u64())
            .unwrap_or(0);
        let thoughts = u
            .get("thoughtsTokenCount")
            .and_then(|n| n.as_u64())
            .unwrap_or(0);
        *acc.cached_tokens = cached;
        // promptTokenCount includes cachedContentTokenCount. Cost prices the
        // uncached remainder at the input rate and the cached part at the
        // cache-read rate, so split them here.
        *acc.input_tokens = prompt.saturating_sub(cached);
        *acc.reasoning_tokens = thoughts;
        *acc.output_tokens = candidates + thoughts;
    }
    Ok(())
}

fn is_transient(err: &anyhow::Error) -> bool {
    if is_abort(err) {
        return false;
    }
    let msg = format!("{err:#}").to_ascii_lowercase();
    if msg.contains("not logged in") {
        return false;
    }
    if let Some(rest) = msg.strip_prefix("vertex ")
        .or_else(|| msg.strip_prefix("gemini "))
        && let Some(code) = rest.split(':').next()
    {
        let code = code.trim();
        if code.starts_with('5') || code == "429" {
            return true;
        }
        if code.starts_with('4') {
            return false;
        }
    }
    msg.contains("timed out")
        || msg.contains("connection reset")
        || msg.contains("connection closed")
        || msg.contains("unexpected eof")
        || msg.contains("error reading a body")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_maps_effort() {
        assert_eq!(thinking_level("low"), "LOW");
        assert_eq!(thinking_level("medium"), "MEDIUM");
        assert_eq!(thinking_level("high"), "HIGH");
        assert_eq!(thinking_budget("gemini-2.5-pro", "high"), 32768);
        assert_eq!(thinking_budget("gemini-2.5-flash", "low"), 2048);
        assert!(uses_thinking_level("gemini-3-pro"));
        assert!(uses_thinking_level("gemini-4-argon"));
        assert!(!uses_thinking_level("gemini-2.5-pro"));
    }

    #[test]
    fn argon_body_sends_level_not_budget_and_inline_pdf() {
        use crate::session::Image;
        let entry = Entry::User {
            text: "read this".into(),
            images: vec![Image {
                media: "pdf".into(),
                data: "JVBERg==".into(),
                name: "notes.pdf".into(),
                kind: "pdf".into(),
            }],
        };
        let body = build_body("gemini-4-argon", "sys", &[entry], &[], "minimal");
        let cfg = &body["generationConfig"];
        assert_eq!(cfg["thinkingConfig"]["thinkingLevel"], "LOW");
        assert!(cfg["thinkingConfig"].get("thinkingBudget").is_none());
        assert_eq!(cfg["maxOutputTokens"], 65536);
        let part = &body["contents"][0]["parts"][1]["inlineData"];
        assert_eq!(part["mimeType"], "application/pdf");
        assert_eq!(part["data"], "JVBERg==");
    }

    #[test]
    fn sse_and_ndjson_split_into_objects() {
        let block = "event: message\ndata: {\"a\":1}\n";
        let payload = line_payload(block.lines().nth(1).unwrap());
        assert_eq!(split_json_objects(&payload), vec!["{\"a\":1}"]);
        let ndjson = "{\"a\":1}\n{\"b\":2}\n";
        let mut objs = Vec::new();
        for line in ndjson.lines() {
            let payload = line_payload(line);
            objs.extend(split_json_objects(&payload).into_iter().map(str::to_string));
        }
        assert_eq!(objs, vec!["{\"a\":1}".to_string(), "{\"b\":2}".to_string()]);
    }

    #[test]
    fn function_call_chunk() {
        let data = r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"read","args":{"path":"a.rs"}}}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":4,"cachedContentTokenCount":2,"thoughtsTokenCount":1}}"#;
        let mut text = String::new();
        let mut thinking = String::new();
        let mut calls = Vec::new();
        let mut finish = String::new();
        let mut input = 0;
        let mut output = 0;
        let mut cached = 0;
        let mut reasoning = 0;
        let mut n = 0;
        let mut pending_sig = String::new();
        let mut started = false;
        apply_chunk(
            data,
            &mut ChunkAcc {
                text: &mut text,
                thinking: &mut thinking,
                calls: &mut calls,
                finish: &mut finish,
                input_tokens: &mut input,
                output_tokens: &mut output,
                cached_tokens: &mut cached,
                reasoning_tokens: &mut reasoning,
                call_n: &mut n,
                pending_sig: &mut pending_sig,
                sink: &mut StreamSink {
                    started: &mut started,
                    on_delta: &mut (|_| {}),
                    on_think: &mut (|_| {}),
                },
            },
        )
        .unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[0].args["path"], "a.rs");
        assert_eq!(finish, "STOP");
        assert_eq!(input, 8);
        assert_eq!(cached, 2);
        assert_eq!(output, 5);
        assert_eq!(reasoning, 1);
    }

    #[test]
    fn thought_signature_is_kept_and_replayed() {
        let first = r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"read","args":{"path":"a.rs"}}}]}}]}"#;
        let second = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"","thoughtSignature":"sig-1"}]},"finishReason":"STOP"}]}"#;
        let mut text = String::new();
        let mut thinking = String::new();
        let mut calls = Vec::new();
        let mut finish = String::new();
        let mut input = 0;
        let mut output = 0;
        let mut cached = 0;
        let mut reasoning = 0;
        let mut n = 0;
        let mut pending = String::new();
        let mut started = false;
        let mut acc = ChunkAcc {
            text: &mut text,
            thinking: &mut thinking,
            calls: &mut calls,
            finish: &mut finish,
            input_tokens: &mut input,
            output_tokens: &mut output,
            cached_tokens: &mut cached,
            reasoning_tokens: &mut reasoning,
            call_n: &mut n,
            pending_sig: &mut pending,
            sink: &mut StreamSink {
                started: &mut started,
                on_delta: &mut (|_| {}),
                on_think: &mut (|_| {}),
            },
        };
        for data in [first, second] {
            apply_chunk(data, &mut acc).unwrap();
        }
        assert_eq!(calls[0].thought_signature, "sig-1");
        let entry = Entry::Assistant {
            text: String::new(),
            thinking: String::new(),
            calls: calls.clone(),
        };
        let body = build_body("gemini-4-argon", "", &[entry], &[], "low");
        let part = &body["contents"][0]["parts"][0];
        assert_eq!(part["functionCall"]["name"], "read");
        assert_eq!(part["thoughtSignature"], "sig-1");

        // Also test when thoughtSignature arrives on a thought part *before* functionCall.
        calls.clear();
        pending.clear();
        let pre_sig = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"thinking","thought":true,"thoughtSignature":"sig-pre"}]}}]}"#;
        let post_fc = r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"bash","args":{"cmd":"ls"}}}]},"finishReason":"STOP"}]}"#;
        let mut acc = ChunkAcc {
            text: &mut text,
            thinking: &mut thinking,
            calls: &mut calls,
            finish: &mut finish,
            input_tokens: &mut input,
            output_tokens: &mut output,
            cached_tokens: &mut cached,
            reasoning_tokens: &mut reasoning,
            call_n: &mut n,
            pending_sig: &mut pending,
            sink: &mut StreamSink {
                started: &mut started,
                on_delta: &mut (|_| {}),
                on_think: &mut (|_| {}),
            },
        };
        for data in [pre_sig, post_fc] {
            apply_chunk(data, &mut acc).unwrap();
        }
        assert_eq!(calls[0].thought_signature, "sig-pre");
    }

    #[test]
    fn parallel_function_responses_share_one_user_turn() {
        let entries = vec![
            Entry::Assistant {
                text: String::new(),
                thinking: String::new(),
                calls: vec![
                    Call {
                        id: "1".into(),
                        name: "read".into(),
                        args: json!({"path": "a.rs"}),
                        thought_signature: "sig-a".into(),
                        restored: false,
                    },
                    Call {
                        id: "2".into(),
                        name: "bash".into(),
                        args: json!({"cmd": "ls"}),
                        thought_signature: "sig-b".into(),
                        restored: false,
                    },
                ],
            },
            Entry::Tool(ToolResult {
                id: "1".into(),
                name: "read".into(),
                content: "file".into(),
                is_error: false,
                restored: false,
            }),
            Entry::Tool(ToolResult {
                id: "2".into(),
                name: "bash".into(),
                content: "ok".into(),
                is_error: false,
                restored: false,
            }),
        ];
        let body = build_body("gemini-4-argon", "", &entries, &[], "low");
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0]["parts"].as_array().unwrap().len(), 2);
        assert_eq!(contents[1]["role"], "user");
        let responses = contents[1]["parts"].as_array().unwrap();
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0]["functionResponse"]["name"], "read");
        assert_eq!(responses[1]["functionResponse"]["name"], "bash");
    }

    #[test]
    fn restored_write_stub_is_a_note_not_a_function_call() {
        let entries = vec![
            Entry::Assistant {
                text: String::new(),
                thinking: String::new(),
                calls: vec![Call {
                    id: "w1".into(),
                    name: "write".into(),
                    args: json!({"path": "src/ui.rs"}),
                    thought_signature: String::new(),
                    restored: true,
                }],
            },
            Entry::Tool(ToolResult {
                id: "w1".into(),
                name: "write".into(),
                content: "wrote 100 bytes".into(),
                is_error: false,
                restored: true,
            }),
        ];
        let body = build_body("gemini-4-argon", "", &entries, &[], "low");
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1, "stub must not demand a functionResponse");
        let parts = contents[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        let note = parts[0]["text"].as_str().unwrap();
        assert!(note.contains("write src/ui.rs"), "{note}");
        assert!(parts[0].get("functionCall").is_none());
    }

    #[test]
    fn jwt_b64url_no_padding() {
        assert_eq!(b64url(b"hello"), "aGVsbG8");
    }

    #[test]
    fn rsa_sign_matches_pkcs1() {
        let dir = std::env::temp_dir().join(format!("fun-rsa-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let key_path = dir.join("key.pem");
        let msg_path = dir.join("msg");
        let sig_path = dir.join("sig");
        let status = std::process::Command::new("openssl")
            .args(["genrsa", "-out"])
            .arg(&key_path)
            .arg("512")
            .output();
        let Ok(status) = status else {
            return;
        };
        if !status.status.success() {
            return;
        }
        std::fs::write(&msg_path, b"vertex").unwrap();
        let sign = std::process::Command::new("openssl")
            .args(["dgst", "-sha256", "-sign"])
            .arg(&key_path)
            .arg("-out")
            .arg(&sig_path)
            .arg(&msg_path)
            .status()
            .unwrap();
        assert!(sign.success());
        let pem = std::fs::read_to_string(&key_path).unwrap();
        let sig = rsa_sha256(&pem, b"vertex").unwrap();
        let expect = std::fs::read(&sig_path).unwrap();
        assert_eq!(sig, expect);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn token_uri_must_be_google() {
        assert!(https_google_url("https://oauth2.googleapis.com/token"));
        assert!(https_google_url(
            "https://oauth2.googleapis.com/token?foo=1"
        ));
        assert!(!https_google_url("http://oauth2.googleapis.com/token"));
        assert!(!https_google_url("https://evil.example/token"));
        assert!(!https_google_url("https://googleapis.com.evil/token"));
        assert!(!https_google_url("https://evil.googleapis.com.evil/token"));
        assert!(!https_google_url(
            "https://user@oauth2.googleapis.com/token"
        ));
        assert!(!https_google_url(
            "https://oauth2.googleapis.com:8443/token"
        ));
        assert!(!https_google_url("https://.googleapis.com/token"));
        assert!(!https_google_url("not a url"));
    }

    #[test]
    fn global_host() {
        assert_eq!(host("global"), "https://aiplatform.googleapis.com");
        assert_eq!(
            host("us-central1"),
            "https://us-central1-aiplatform.googleapis.com"
        );
    }
}
