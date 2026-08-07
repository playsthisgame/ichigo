//! Converts between a `RequestConfig` and a cURL command, in both directions.
//!
//! Pure string work: no IO, no clipboard, no TUI types. `to_curl` takes the same
//! two inputs as `utils::send_request` — a config and a resolved variable map —
//! and must stay in step with it, since a command that disagrees with what
//! ichigo actually sends is worse than no command at all. `from_curl` is its
//! inverse, parsing a pasted command into a config.
//!
//! Substitution is shared (`utils::interpolate`). The rules duplicated from
//! `send_request`, and therefore the real drift risk, are:
//!   * query parameters are folded into the URL rather than sent separately
//!   * a body contributes a `Content-Type` header on top of `config.headers`
//!
//! Both of those are also what the two directions here have to agree on, and
//! they are the reason `from_curl` normalizes rather than transcribing:
//!   * a `Content-Type` header on a command with a body is stored as
//!     `body.content_type`, never in `headers` — `to_curl` re-derives that
//!     header from the body, so a config holding it in both places renders it
//!     twice
//!   * a query string on the URL is lifted into `query`, undoing the folding
//!
//! The round-trip tests at the bottom of this file are what keep the pair
//! honest; they are the reason both functions live in one module.

use std::collections::HashMap;

use anyhow::{anyhow, bail, Result};
use reqwest::Url;

use crate::config::{Body, RequestConfig};
use crate::utils::interpolate;

/// Builds a multi-line cURL command equivalent to the request `send_request`
/// would issue for this config and these variables.
pub fn to_curl(config: &RequestConfig, vars: &HashMap<String, String>) -> String {
    let method = config.method.to_uppercase();

    // curl already defaults to GET; spelling it out is noise.
    let mut head = String::from("curl");
    if method != "GET" {
        head.push_str(&format!(" -X {}", method));
    }
    head.push(' ');
    head.push_str(&shell_quote(&build_url(config, vars)));

    let mut parts = vec![head];

    // Headers come out of a HashMap, so iteration order is arbitrary. Sort:
    // a command that differs between invocations can't be diffed or tested.
    let mut headers: Vec<(String, String)> = config
        .headers
        .iter()
        .map(|(k, v)| (interpolate(k, vars), interpolate(v, vars)))
        .collect();
    headers.sort();
    for (name, value) in headers {
        parts.push(format!("-H {}", shell_quote(&format!("{}: {}", name, value))));
    }

    if let Some(body) = &config.body {
        // Mirrors send_request, which sets Content-Type from the body in
        // addition to whatever `headers` already carries.
        parts.push(format!(
            "-H {}",
            shell_quote(&format!("Content-Type: {}", body.content_type))
        ));
        // --data-raw rather than -d: -d strips newlines (corrupting multi-line
        // JSON) and reads a leading '@' as a filename.
        parts.push(format!(
            "--data-raw {}",
            shell_quote(&interpolate(&body.data, vars))
        ));
    }

    parts.join(" \\\n  ")
}

/// Interpolates the URL and folds the query map into it, matching how
/// `send_request` hands the query to reqwest.
fn build_url(config: &RequestConfig, vars: &HashMap<String, String>) -> String {
    let base = interpolate(&config.url, vars);
    if config.query.is_empty() {
        return base;
    }

    let mut query: Vec<(String, String)> = config
        .query
        .iter()
        .map(|(k, v)| (interpolate(k, vars), interpolate(v, vars)))
        .collect();
    query.sort();

    // Url::parse normalises as it goes — lowercasing the host among other
    // things — which would rewrite an unresolved `{{HOST}}` to `{{host}}` and
    // leave the user a placeholder they can't find. A URL still carrying
    // placeholders can't be sent anyway, so keep it verbatim.
    let parsed = if base.contains("{{") { None } else { Url::parse(&base).ok() };

    match parsed {
        Some(mut url) => {
            {
                let mut pairs = url.query_pairs_mut();
                for (k, v) in &query {
                    pairs.append_pair(k, v);
                }
            }
            url.to_string()
        }
        // Placeholder-bearing or genuinely unparseable. Silently dropping the
        // query would make the command wrong in a way that's hard to notice,
        // so encode it through the same serializer against a dummy base and
        // graft it onto the URL as written.
        None => {
            let mut dummy = Url::parse("http://placeholder.invalid/").expect("static URL parses");
            {
                let mut pairs = dummy.query_pairs_mut();
                for (k, v) in &query {
                    pairs.append_pair(k, v);
                }
            }
            let encoded = dummy.query().unwrap_or_default();
            let separator = if base.contains('?') { '&' } else { '?' };
            format!("{}{}{}", base, separator, encoded)
        }
    }
}

/// Wraps a value in single quotes so the shell reproduces it literally.
///
/// Single quotes suppress every form of expansion, so the only character
/// needing care is a single quote itself: close, emit an escaped one, reopen.
/// Quoting is unconditional — deciding a value "looks safe" is a bug generator.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// ─── Parsing ──────────────────────────────────────────────────────────────────

/// Parses a cURL command into a `RequestConfig`.
///
/// The `name` field is left empty: a command does not carry one, and the caller
/// — the CLI argument or the TUI form — is where it comes from. `description`,
/// `profiles`, and `extract` are likewise not representable in a command.
///
/// Unrecognized flags are an error rather than a silent skip. A parser that
/// ignores what it does not understand turns `curl -F file=@x URL` into a
/// bodyless GET, and the user finds out when the request fails against a real
/// server.
pub fn from_curl(input: &str) -> Result<RequestConfig> {
    let tokens = tokenize(strip_prompt(input.trim()))?;
    let mut tokens = tokens.into_iter();
    match tokens.next() {
        Some(first) if first == "curl" => {}
        Some(first) => bail!("not a curl command (starts with `{first}`)"),
        None => bail!("no command to parse"),
    }
    let tokens: Vec<String> = tokens.collect();

    let mut parts = Parts::default();
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i].clone();
        i += 1;

        if let Some(long) = token.strip_prefix("--") {
            // `--flag=value` and `--flag value` are both accepted.
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value.to_string())),
                None => (long, None),
            };
            let display = format!("--{name}");
            apply_flag(name, &display, inline, &tokens, &mut i, &mut parts)?;
        } else if token.len() > 1 && token.starts_with('-') {
            apply_short_bundle(&token[1..], &tokens, &mut i, &mut parts)?;
        } else {
            parts.set_url(&token)?;
        }
    }

    parts.finish()
}

/// People copy the prompt along with the command more often than not.
fn strip_prompt(input: &str) -> &str {
    input
        .strip_prefix("$ ")
        .or_else(|| input.strip_prefix("% "))
        .unwrap_or(input)
}

/// What the flags collected so far add up to, before the derivations that turn
/// them into a config.
#[derive(Default)]
struct Parts {
    url: Option<String>,
    method: Option<String>,
    headers: Vec<(String, String)>,
    data: Vec<String>,
    get: bool,
    head: bool,
}

impl Parts {
    fn set_url(&mut self, url: &str) -> Result<()> {
        if let Some(existing) = &self.url {
            bail!("more than one URL in the command (`{existing}` and `{url}`); a config holds one");
        }
        self.url = Some(url.to_string());
        Ok(())
    }

    /// Appends a header, combining a repeat of the same name with `, ` per the
    /// HTTP field-value rule. `headers` is a map, so the alternative to
    /// combining is dropping one of them.
    fn push_header(&mut self, name: &str, value: &str) {
        match self.find_header(name) {
            Some(idx) => {
                let existing = &mut self.headers[idx].1;
                existing.push_str(", ");
                existing.push_str(value);
            }
            None => self.headers.push((name.to_string(), value.to_string())),
        }
    }

    /// Used for the headers `--json` implies: an explicit `-H` wins.
    fn push_header_if_absent(&mut self, name: &str, value: &str) {
        if self.find_header(name).is_none() {
            self.headers.push((name.to_string(), value.to_string()));
        }
    }

    fn find_header(&self, name: &str) -> Option<usize> {
        self.headers
            .iter()
            .position(|(k, _)| k.eq_ignore_ascii_case(name))
    }

    fn take_header(&mut self, name: &str) -> Option<String> {
        self.find_header(name).map(|idx| self.headers.remove(idx).1)
    }

    fn finish(mut self) -> Result<RequestConfig> {
        let mut url = self
            .url
            .clone()
            .ok_or_else(|| anyhow!("no URL found in the command"))?;

        let has_data = !self.data.is_empty();
        let data = self.data.join("&");

        // `-G` moves the data onto the query string and leaves a GET behind.
        let body_data = if self.get {
            if !data.is_empty() {
                let separator = if url.contains('?') { '&' } else { '?' };
                url = format!("{url}{separator}{data}");
            }
            None
        } else if has_data {
            Some(data)
        } else {
            None
        };

        // curl's own rules: an explicit -X wins, -I is a HEAD, data implies a
        // POST. Defaulting to GET instead would silently rewrite every POST
        // whose source command left -X off, which is most of them.
        let method = match &self.method {
            Some(m) => m.to_uppercase(),
            None if self.head => "HEAD".to_string(),
            None if body_data.is_some() => "POST".to_string(),
            None => "GET".to_string(),
        };

        // Content-Type belongs to the body, not to `headers` — see the module
        // doc. Without a body there is nowhere to put it, so it stays a header.
        let body = body_data.map(|data| {
            let content_type = self
                .take_header("Content-Type")
                .unwrap_or_else(|| "application/x-www-form-urlencoded".to_string());
            Body { content_type, data }
        });

        let (url, query) = lift_query(&url);

        Ok(RequestConfig {
            name: String::new(),
            method,
            url,
            description: None,
            headers: self.headers.into_iter().collect(),
            query,
            body,
            extract: None,
            profiles: None,
        })
    }
}

/// Expands a run of short flags (`-sSL`, `-XPOST`, `-H'X: 1'`) one letter at a
/// time. Splitting rather than matching the bundle whole is what makes an
/// unsupported letter inside an otherwise fine bundle an error.
fn apply_short_bundle(
    bundle: &str,
    tokens: &[String],
    i: &mut usize,
    parts: &mut Parts,
) -> Result<()> {
    let chars: Vec<char> = bundle.chars().collect();
    for (pos, ch) in chars.iter().enumerate() {
        let name = short_to_long(*ch)
            .ok_or_else(|| anyhow!("unsupported curl flag: -{ch}"))?;
        let display = format!("-{ch}");

        if takes_value(name) {
            // The rest of the bundle is the value when there is one.
            let rest: String = chars[pos + 1..].iter().collect();
            let inline = if rest.is_empty() { None } else { Some(rest) };
            apply_flag(name, &display, inline, tokens, i, parts)?;
            return Ok(());
        }
        apply_flag(name, &display, None, tokens, i, parts)?;
    }
    Ok(())
}

fn short_to_long(ch: char) -> Option<&'static str> {
    Some(match ch {
        'X' => "request",
        'H' => "header",
        'd' => "data",
        'G' => "get",
        'I' => "head",
        'u' => "user",
        'b' => "cookie",
        'A' => "user-agent",
        'e' => "referer",
        'o' => "output",
        'm' => "max-time",
        'w' => "write-out",
        's' => "silent",
        'S' => "show-error",
        'v' => "verbose",
        'i' => "include",
        'k' => "insecure",
        'L' => "location",
        'f' => "fail",
        'g' => "globoff",
        '#' => "progress-bar",
        _ => return None,
    })
}

fn takes_value(name: &str) -> bool {
    matches!(
        name,
        "url"
            | "request"
            | "header"
            | "data"
            | "data-raw"
            | "data-ascii"
            | "data-binary"
            | "data-urlencode"
            | "json"
            | "user"
            | "cookie"
            | "user-agent"
            | "referer"
            | "output"
            | "max-time"
            | "connect-timeout"
            | "write-out"
            | "retry"
            | "limit-rate"
    )
}

/// Flags that describe how curl behaves rather than what it sends. Accepting
/// and dropping these is what lets a devtools "copy as cURL" paste through.
///
/// `--compressed` is the interesting one: it does add an `Accept-Encoding`
/// header, but reqwest negotiates encoding itself, so storing the header would
/// make the config claim something `send_request` does not do.
fn is_ignored(name: &str) -> bool {
    matches!(
        name,
        "silent"
            | "show-error"
            | "verbose"
            | "include"
            | "insecure"
            | "location"
            | "compressed"
            | "progress-bar"
            | "no-progress-meter"
            | "fail"
            | "globoff"
            | "output"
            | "max-time"
            | "connect-timeout"
            | "write-out"
            | "retry"
            | "limit-rate"
    )
}

fn apply_flag(
    name: &str,
    display: &str,
    inline: Option<String>,
    tokens: &[String],
    i: &mut usize,
    parts: &mut Parts,
) -> Result<()> {
    let mut value = || -> Result<String> {
        if let Some(v) = inline.clone() {
            return Ok(v);
        }
        let v = tokens
            .get(*i)
            .ok_or_else(|| anyhow!("`{display}` expects a value"))?
            .clone();
        *i += 1;
        Ok(v)
    };

    match name {
        "url" => parts.set_url(&value()?)?,
        "request" => parts.method = Some(value()?),
        "header" => {
            let raw = value()?;
            let (n, v) = split_header(&raw)?;
            parts.push_header(&n, &v);
        }
        // Only the `-d` family reads a leading `@` as a filename; `--data-raw`
        // exists precisely so it does not, so a literal `@` survives there.
        "data" | "data-ascii" | "data-binary" => {
            let raw = value()?;
            if let Some(file) = raw.strip_prefix('@') {
                bail!("`{display} @{file}` reads a file; inline the data instead");
            }
            parts.data.push(raw);
        }
        "data-raw" => parts.data.push(value()?),
        "data-urlencode" => {
            let raw = value()?;
            parts.data.push(urlencode_data(&raw, display)?);
        }
        "json" => {
            parts.data.push(value()?);
            parts.push_header_if_absent("Content-Type", "application/json");
            parts.push_header_if_absent("Accept", "application/json");
        }
        "get" => parts.get = true,
        "head" => parts.head = true,
        "user" => {
            let raw = value()?;
            if !raw.contains(':') {
                bail!("`{display}` needs `user:password`; curl prompts for a missing password, which a parser cannot do");
            }
            let encoded = base64_encode(raw.as_bytes());
            parts.push_header("Authorization", &format!("Basic {encoded}"));
        }
        "cookie" => {
            let raw = value()?;
            if raw.contains('=') || raw.contains(';') {
                parts.push_header("Cookie", &raw);
            } else {
                bail!("`{display} {raw}` reads a cookie file; inline the cookies instead");
            }
        }
        "user-agent" => {
            let raw = value()?;
            parts.push_header("User-Agent", &raw);
        }
        "referer" => {
            let raw = value()?;
            parts.push_header("Referer", &raw);
        }
        _ if is_ignored(name) => {
            if takes_value(name) {
                value()?;
            }
        }
        _ => bail!("unsupported curl flag: {display}"),
    }
    Ok(())
}

/// `Name: value`, or `Name;` for curl's "send this header empty" form.
fn split_header(raw: &str) -> Result<(String, String)> {
    if let Some((name, value)) = raw.split_once(':') {
        return Ok((name.trim().to_string(), value.trim().to_string()));
    }
    if let Some(name) = raw.strip_suffix(';') {
        return Ok((name.trim().to_string(), String::new()));
    }
    bail!("malformed header `{raw}` (expected `Name: value`)")
}

/// curl's `--data-urlencode`, minus the `@file` forms.
fn urlencode_data(raw: &str, display: &str) -> Result<String> {
    if raw.starts_with('@') || raw.split_once('@').is_some_and(|(n, _)| !n.contains('=')) {
        bail!("`{display}` with an `@file` reference is not supported; inline the data instead");
    }
    // Percent-encode through the same serializer `to_curl` folds queries with,
    // so the two directions agree on escaping.
    let (name, content) = match raw.split_once('=') {
        Some((name, content)) => (name, content),
        None => ("", raw),
    };
    let mut dummy = Url::parse("http://placeholder.invalid/").expect("static URL parses");
    dummy.query_pairs_mut().append_pair(name, content);
    let encoded = dummy.query().unwrap_or_default().to_string();
    // `append_pair("", v)` yields `=v`; curl emits a bare value for that form.
    Ok(match name.is_empty() {
        true => encoded.trim_start_matches('=').to_string(),
        false => encoded,
    })
}

/// Splits a query string off the URL and decodes it into the `query` map,
/// undoing the folding `to_curl` does.
///
/// Bails out — leaving the query on the URL, which `send_request` handles
/// correctly — when the result would lose information: a repeated key cannot
/// survive a `HashMap`, and a URL carrying `{{VAR}}` placeholders or a fragment
/// is not something to take apart and reassemble.
fn lift_query(url: &str) -> (String, HashMap<String, String>) {
    let unchanged = || (url.to_string(), HashMap::new());

    let Some((base, query)) = url.split_once('?') else {
        return unchanged();
    };
    if query.is_empty() || url.contains("{{") || query.contains('#') {
        return unchanged();
    }

    // Decode through `Url` rather than splitting by hand so `+` becomes a space
    // and percent-escapes resolve exactly as `to_curl`'s serializer wrote them.
    let Ok(parsed) = Url::parse(&format!("http://placeholder.invalid/?{query}")) else {
        return unchanged();
    };

    let mut map = HashMap::new();
    for (k, v) in parsed.query_pairs() {
        if map.insert(k.into_owned(), v.into_owned()).is_some() {
            return unchanged(); // repeated key: keep every value, on the URL
        }
    }
    (base.to_string(), map)
}

/// Standard base64. Present for `-u` alone, which is a small enough need that a
/// dependency would cost more than the twenty lines.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

// ─── Tokenizing ───────────────────────────────────────────────────────────────

/// Splits a command into tokens using shell quoting rules.
///
/// Only the quoting a cURL command actually uses: this is deliberately not a
/// shell, so `$(…)`, pipes, and variable expansion are left as literal text.
fn tokenize(input: &str) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    // Tracked separately from `current.is_empty()` so an empty quoted token
    // (`''`, which is how an empty body arrives) survives.
    let mut started = false;
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if started {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            '\\' => match chars.next() {
                // Line continuation: the reason a pasted command is one command.
                Some('\n') => {}
                Some('\r') => {
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                }
                Some(other) => {
                    current.push(other);
                    started = true;
                }
                None => bail!("command ends with a trailing backslash"),
            },
            '\'' => {
                read_single_quoted(&mut chars, &mut current)?;
                started = true;
            }
            '"' => {
                read_double_quoted(&mut chars, &mut current)?;
                started = true;
            }
            // ANSI-C quoting: Chrome emits this for bodies with control
            // characters or non-ASCII text.
            '$' if chars.peek() == Some(&'\'') => {
                chars.next();
                read_ansi_c_quoted(&mut chars, &mut current)?;
                started = true;
            }
            other => {
                current.push(other);
                started = true;
            }
        }
    }

    if started {
        tokens.push(current);
    }
    Ok(tokens)
}

type Chars<'a> = std::iter::Peekable<std::str::Chars<'a>>;

/// Single quotes suppress everything, including backslashes — which is why
/// `shell_quote` closes and reopens them to emit one.
fn read_single_quoted(chars: &mut Chars<'_>, out: &mut String) -> Result<()> {
    for c in chars.by_ref() {
        if c == '\'' {
            return Ok(());
        }
        out.push(c);
    }
    bail!("unterminated single quote")
}

fn read_double_quoted(chars: &mut Chars<'_>, out: &mut String) -> Result<()> {
    while let Some(c) = chars.next() {
        match c {
            '"' => return Ok(()),
            '\\' => match chars.next() {
                // The only four the shell unescapes inside double quotes;
                // anything else keeps its backslash.
                Some(e @ ('"' | '\\' | '`' | '$')) => out.push(e),
                Some('\n') => {}
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => bail!("unterminated double quote"),
            },
            other => out.push(other),
        }
    }
    bail!("unterminated double quote")
}

/// `$'…'`. Accumulates bytes rather than chars so that a run of `\xNN` escapes
/// spelling out a multi-byte UTF-8 character reassembles into that character.
fn read_ansi_c_quoted(chars: &mut Chars<'_>, out: &mut String) -> Result<()> {
    let mut bytes: Vec<u8> = Vec::new();
    let mut closed = false;

    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                closed = true;
                break;
            }
            '\\' => match chars.next() {
                Some('n') => bytes.push(b'\n'),
                Some('t') => bytes.push(b'\t'),
                Some('r') => bytes.push(b'\r'),
                Some('a') => bytes.push(0x07),
                Some('b') => bytes.push(0x08),
                Some('f') => bytes.push(0x0c),
                Some('v') => bytes.push(0x0b),
                Some('e') => bytes.push(0x1b),
                Some('\\') => bytes.push(b'\\'),
                Some('\'') => bytes.push(b'\''),
                Some('"') => bytes.push(b'"'),
                Some('x') => {
                    let mut hex = String::new();
                    while hex.len() < 2 && chars.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                        hex.push(chars.next().expect("peeked"));
                    }
                    if hex.is_empty() {
                        bail!("`\\x` with no hex digits in a $'…' string");
                    }
                    bytes.push(u8::from_str_radix(&hex, 16).expect("hex digits parse"));
                }
                Some(other) => {
                    let mut buf = [0u8; 4];
                    bytes.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
                }
                None => bail!("unterminated $'…' string"),
            },
            other => {
                let mut buf = [0u8; 4];
                bytes.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
        }
    }

    if !closed {
        bail!("unterminated $'…' string");
    }
    let decoded = String::from_utf8(bytes)
        .map_err(|_| anyhow!("$'…' string is not valid UTF-8 and cannot be stored in a config"))?;
    out.push_str(&decoded);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: &str, url: &str) -> RequestConfig {
        RequestConfig {
            name: "t".to_string(),
            method: method.to_string(),
            url: url.to_string(),
            description: None,
            headers: HashMap::new(),
            query: HashMap::new(),
            body: None,
            extract: None,
            profiles: None,
        }
    }

    fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn get_omits_redundant_method_flag() {
        let out = to_curl(&req("GET", "https://api/x"), &HashMap::new());
        assert!(!out.contains("-X GET"), "got: {out}");
        assert!(out.starts_with("curl 'https://api/x'"), "got: {out}");
    }

    #[test]
    fn non_get_includes_method() {
        let out = to_curl(&req("POST", "https://api/x"), &HashMap::new());
        assert!(out.starts_with("curl -X POST 'https://api/x'"), "got: {out}");
    }

    #[test]
    fn method_is_uppercased() {
        let out = to_curl(&req("delete", "https://api/x"), &HashMap::new());
        assert!(out.contains("-X DELETE"), "got: {out}");
    }

    #[test]
    fn headers_are_emitted_and_sorted() {
        let mut c = req("GET", "https://api/x");
        c.headers.insert("Zeta".to_string(), "1".to_string());
        c.headers.insert("Accept".to_string(), "application/json".to_string());
        c.headers.insert("Middle".to_string(), "2".to_string());
        let out = to_curl(&c, &HashMap::new());
        let accept = out.find("-H 'Accept:").unwrap();
        let middle = out.find("-H 'Middle:").unwrap();
        let zeta = out.find("-H 'Zeta:").unwrap();
        assert!(accept < middle && middle < zeta, "not sorted: {out}");
    }

    #[test]
    fn output_is_stable_across_calls() {
        let mut c = req("GET", "https://api/x");
        for (k, v) in [("A", "1"), ("B", "2"), ("C", "3"), ("D", "4"), ("E", "5")] {
            c.headers.insert(k.to_string(), v.to_string());
            c.query.insert(k.to_lowercase(), v.to_string());
        }
        assert_eq!(to_curl(&c, &HashMap::new()), to_curl(&c, &HashMap::new()));
    }

    #[test]
    fn query_params_are_folded_into_url() {
        let mut c = req("GET", "https://api/x");
        c.query.insert("page".to_string(), "2".to_string());
        let out = to_curl(&c, &HashMap::new());
        assert!(out.contains("https://api/x?page=2"), "got: {out}");
        // Not emitted as a separate flag.
        assert!(!out.contains("--data-urlencode"), "got: {out}");
    }

    #[test]
    fn query_params_are_percent_encoded() {
        let mut c = req("GET", "https://api/x");
        c.query.insert("q".to_string(), "a b&c=d".to_string());
        c.query.insert("né".to_string(), "café".to_string());
        let out = to_curl(&c, &HashMap::new());
        // Raw separators must not leak through unencoded.
        assert!(out.contains("a+b%26c%3Dd"), "got: {out}");
        assert!(out.contains("caf%C3%A9"), "got: {out}");
    }

    #[test]
    fn query_appends_to_existing_query_string() {
        let mut c = req("GET", "https://api/x?existing=1");
        c.query.insert("added".to_string(), "2".to_string());
        let out = to_curl(&c, &HashMap::new());
        assert!(out.contains("existing=1"), "got: {out}");
        assert!(out.contains("added=2"), "got: {out}");
    }

    #[test]
    fn unparseable_url_keeps_its_query() {
        // An unresolved placeholder in the host: the query must not vanish.
        let mut c = req("GET", "http://{{HOST}}/x");
        c.query.insert("page".to_string(), "2".to_string());
        let out = to_curl(&c, &HashMap::new());
        assert!(out.contains("{{HOST}}"), "got: {out}");
        assert!(out.contains("page=2"), "got: {out}");
    }

    #[test]
    fn body_carries_content_type() {
        let mut c = req("POST", "https://api/x");
        c.body = Some(Body {
            content_type: "application/json".to_string(),
            data: r#"{"name":"ada"}"#.to_string(),
        });
        let out = to_curl(&c, &HashMap::new());
        assert!(out.contains("-H 'Content-Type: application/json'"), "got: {out}");
        assert!(out.contains(r#"--data-raw '{"name":"ada"}'"#), "got: {out}");
    }

    #[test]
    fn multiline_body_is_preserved() {
        let mut c = req("POST", "https://api/x");
        c.body = Some(Body {
            content_type: "application/json".to_string(),
            data: "{\n  \"a\": 1\n}".to_string(),
        });
        let out = to_curl(&c, &HashMap::new());
        assert!(out.contains("{\n  \"a\": 1\n}"), "newlines collapsed: {out}");
    }

    #[test]
    fn single_quotes_are_escaped() {
        let mut c = req("GET", "https://api/x");
        c.headers.insert("X-Note".to_string(), "it's here".to_string());
        let out = to_curl(&c, &HashMap::new());
        // Close, escaped quote, reopen — the only escape single quotes allow.
        assert!(out.contains(r"'X-Note: it'\''s here'"), "got: {out}");
    }

    #[test]
    fn shell_metacharacters_are_not_expandable() {
        let mut c = req("GET", "https://api/x");
        c.headers.insert("A".to_string(), "$HOME `id` a b".to_string());
        let out = to_curl(&c, &HashMap::new());
        // Inside single quotes none of these expand.
        assert!(out.contains("-H 'A: $HOME `id` a b'"), "got: {out}");
    }

    #[test]
    fn placeholders_are_substituted_from_vars() {
        let mut c = req("GET", "https://{{HOST}}/x");
        c.headers.insert("Authorization".to_string(), "Bearer {{TOKEN}}".to_string());
        c.query.insert("u".to_string(), "{{USER}}".to_string());
        c.body = Some(Body {
            content_type: "application/json".to_string(),
            data: r#"{"t":"{{TOKEN}}"}"#.to_string(),
        });
        let out = to_curl(&c, &vars(&[("HOST", "api.test"), ("TOKEN", "abc123"), ("USER", "ada")]));
        assert!(out.contains("https://api.test/x"), "got: {out}");
        assert!(out.contains("Bearer abc123"), "got: {out}");
        assert!(out.contains("u=ada"), "got: {out}");
        assert!(out.contains(r#"{"t":"abc123"}"#), "got: {out}");
        assert!(!out.contains("{{"), "placeholder left behind: {out}");
    }

    #[test]
    fn header_names_are_interpolated_too() {
        // send_request interpolates keys as well as values.
        let mut c = req("GET", "https://api/x");
        c.headers.insert("X-{{KIND}}".to_string(), "v".to_string());
        let out = to_curl(&c, &vars(&[("KIND", "Trace")]));
        assert!(out.contains("-H 'X-Trace: v'"), "got: {out}");
    }

    #[test]
    fn unresolved_placeholder_stays_literal() {
        let mut c = req("GET", "https://api/x");
        c.headers.insert("A".to_string(), "{{NOPE_NOT_SET_ANYWHERE}}".to_string());
        let out = to_curl(&c, &HashMap::new());
        assert!(out.contains("{{NOPE_NOT_SET_ANYWHERE}}"), "got: {out}");
    }

    #[test]
    fn environment_is_the_fallback() {
        // Matches interpolate's order: vars map first, then the environment.
        unsafe { std::env::set_var("ICHIGO_CURL_TEST_TOKEN", "from-env") };
        let mut c = req("GET", "https://api/x");
        c.headers.insert("A".to_string(), "{{ICHIGO_CURL_TEST_TOKEN}}".to_string());
        let out = to_curl(&c, &HashMap::new());
        assert!(out.contains("-H 'A: from-env'"), "got: {out}");

        // An explicit var still wins over the environment.
        let out = to_curl(&c, &vars(&[("ICHIGO_CURL_TEST_TOKEN", "from-map")]));
        assert!(out.contains("-H 'A: from-map'"), "got: {out}");
        unsafe { std::env::remove_var("ICHIGO_CURL_TEST_TOKEN") };
    }

    #[test]
    fn continuations_join_flags() {
        let mut c = req("POST", "https://api/x");
        c.headers.insert("A".to_string(), "1".to_string());
        let out = to_curl(&c, &HashMap::new());
        assert!(out.contains(" \\\n  -H "), "got: {out}");
    }

    // ─── Tokenizing ──────────────────────────────────────────────────────────

    fn toks(input: &str) -> Vec<String> {
        tokenize(input).expect("tokenizes")
    }

    #[test]
    fn splits_on_unquoted_whitespace() {
        assert_eq!(toks("curl -X POST url"), ["curl", "-X", "POST", "url"]);
    }

    #[test]
    fn line_continuations_join_the_command() {
        let input = "curl -X POST \\\n  'https://api/x' \\\n  -H 'A: 1'";
        assert_eq!(toks(input), ["curl", "-X", "POST", "https://api/x", "-H", "A: 1"]);
    }

    #[test]
    fn single_quotes_are_literal() {
        // Nothing expands and no escape exists inside single quotes.
        assert_eq!(toks(r#"'$HOME `id` \n'"#), [r"$HOME `id` \n"]);
    }

    #[test]
    fn escaped_single_quote_round_trips() {
        // The close/escape/reopen form shell_quote emits.
        assert_eq!(toks(r"'X-Note: it'\''s here'"), ["X-Note: it's here"]);
    }

    #[test]
    fn double_quotes_unescape_the_shell_four() {
        assert_eq!(toks(r#""a\"b\\c\$d\`e""#), [r#"a"b\c$d`e"#]);
    }

    #[test]
    fn double_quotes_keep_other_backslashes() {
        // \n inside double quotes is a literal backslash-n to the shell.
        assert_eq!(toks(r#""a\nb""#), [r"a\nb"]);
    }

    #[test]
    fn ansi_c_quoting_decodes_escapes() {
        assert_eq!(toks(r"$'line1\nline2'"), ["line1\nline2"]);
        assert_eq!(toks(r"$'a\tb\r\\c\'d'"), ["a\tb\r\\c'd"]);
    }

    #[test]
    fn ansi_c_hex_escapes_reassemble_utf8() {
        // Chrome spells non-ASCII as a run of \xNN bytes; decoding per-char
        // would produce mojibake, so the bytes are accumulated first.
        assert_eq!(toks(r"$'caf\xc3\xa9'"), ["café"]);
    }

    #[test]
    fn adjacent_runs_concatenate_into_one_token() {
        assert_eq!(toks("-H'X: 1'"), ["-HX: 1"]);
        assert_eq!(toks(r#"--data-raw'{"a":1}'"#), [r#"--data-raw{"a":1}"#]);
    }

    #[test]
    fn empty_quotes_are_a_token() {
        assert_eq!(toks("-d ''"), ["-d", ""]);
    }

    #[test]
    fn unterminated_quotes_are_errors() {
        assert!(tokenize("'abc").is_err());
        assert!(tokenize(r#""abc"#).is_err());
        assert!(tokenize(r"$'abc").is_err());
        assert!(tokenize(r"abc\").is_err());
    }

    // ─── Parsing ─────────────────────────────────────────────────────────────

    fn parse(cmd: &str) -> RequestConfig {
        from_curl(cmd).unwrap_or_else(|e| panic!("expected `{cmd}` to parse: {e:#}"))
    }

    fn err(cmd: &str) -> String {
        format!("{:#}", from_curl(cmd).expect_err("expected an error"))
    }

    #[test]
    fn parses_a_command_with_headers_and_a_body() {
        let c = parse(
            r#"curl -X POST 'https://api.example.com/users' -H 'Accept: application/json' -H 'Content-Type: application/json' --data-raw '{"name":"ada"}'"#,
        );
        assert_eq!(c.method, "POST");
        assert_eq!(c.url, "https://api.example.com/users");
        assert_eq!(c.headers.get("Accept").unwrap(), "application/json");
        let body = c.body.expect("body");
        assert_eq!(body.content_type, "application/json");
        assert_eq!(body.data, r#"{"name":"ada"}"#);
    }

    #[test]
    fn url_may_be_positional_or_flagged() {
        let a = parse("curl 'https://api.example.com/x'");
        let b = parse("curl --url 'https://api.example.com/x'");
        let c = parse("curl --url='https://api.example.com/x'");
        assert_eq!(a.url, "https://api.example.com/x");
        assert_eq!(b.url, a.url);
        assert_eq!(c.url, a.url);
    }

    #[test]
    fn a_leading_prompt_marker_is_stripped() {
        assert_eq!(parse("$ curl 'https://api/x'").url, "https://api/x");
        assert_eq!(parse("% curl 'https://api/x'").url, "https://api/x");
    }

    #[test]
    fn non_curl_input_is_rejected() {
        assert!(err("wget https://api/x").contains("not a curl command"));
        assert!(from_curl("").is_err());
    }

    #[test]
    fn a_command_with_no_url_is_rejected() {
        assert!(err("curl -X POST").contains("no URL"));
    }

    #[test]
    fn a_second_url_is_rejected() {
        // curl would fetch both; a config holds one.
        assert!(err("curl 'https://a/x' 'https://b/y'").contains("more than one URL"));
    }

    #[test]
    fn repeated_data_flags_concatenate() {
        let c = parse("curl 'https://api/x' -d 'a=1' -d 'b=2'");
        assert_eq!(c.body.expect("body").data, "a=1&b=2");
    }

    #[test]
    fn attached_short_flag_values_are_read() {
        let c = parse("curl -XPUT 'https://api/x'");
        assert_eq!(c.method, "PUT");
    }

    #[test]
    fn short_flag_bundles_are_split() {
        let c = parse("curl -sSL 'https://api/x'");
        assert_eq!(c.method, "GET");
        assert!(c.headers.is_empty());
    }

    #[test]
    fn method_defaults_follow_curl() {
        assert_eq!(parse("curl 'https://api/x'").method, "GET");
        assert_eq!(parse("curl 'https://api/x' -d 'a=1'").method, "POST");
        assert_eq!(parse("curl -I 'https://api/x'").method, "HEAD");
        assert_eq!(parse("curl -X delete 'https://api/x'").method, "DELETE");
        // An explicit method beats every derivation.
        assert_eq!(parse("curl -X PUT 'https://api/x' -d 'a=1'").method, "PUT");
    }

    #[test]
    fn content_type_moves_onto_the_body() {
        let c = parse("curl 'https://api/x' -H 'Content-Type: application/json' --data-raw '{}'");
        assert_eq!(c.body.expect("body").content_type, "application/json");
        assert!(
            !c.headers.keys().any(|k| k.eq_ignore_ascii_case("content-type")),
            "Content-Type left in headers would be emitted twice: {:?}",
            c.headers
        );
    }

    #[test]
    fn content_type_matching_is_case_insensitive() {
        let c = parse("curl 'https://api/x' -H 'content-type: text/plain' -d 'x'");
        assert_eq!(c.body.expect("body").content_type, "text/plain");
        assert!(c.headers.is_empty(), "got: {:?}", c.headers);
    }

    #[test]
    fn a_body_without_a_content_type_gets_curls_default() {
        let c = parse("curl 'https://api/x' -d 'a=1'");
        assert_eq!(
            c.body.expect("body").content_type,
            "application/x-www-form-urlencoded"
        );
    }

    #[test]
    fn a_bodyless_command_keeps_its_content_type_header() {
        let c = parse("curl 'https://api/x' -H 'Content-Type: application/json'");
        assert!(c.body.is_none());
        assert_eq!(c.headers.get("Content-Type").unwrap(), "application/json");
    }

    #[test]
    fn json_flag_sets_content_type_and_accept() {
        let c = parse(r#"curl 'https://api/x' --json '{"a":1}'"#);
        let body = c.body.expect("body");
        assert_eq!(body.content_type, "application/json");
        assert_eq!(body.data, r#"{"a":1}"#);
        assert_eq!(c.headers.get("Accept").unwrap(), "application/json");
        assert_eq!(c.method, "POST");
    }

    #[test]
    fn an_explicit_header_beats_the_json_implied_one() {
        let c = parse(r#"curl 'https://api/x' -H 'Accept: text/plain' --json '{}'"#);
        assert_eq!(c.headers.get("Accept").unwrap(), "text/plain");
    }

    #[test]
    fn repeated_headers_combine() {
        // A map cannot hold both, and dropping one would be silent.
        let c = parse("curl 'https://api/x' -H 'Accept: a' -H 'accept: b'");
        assert_eq!(c.headers.get("Accept").unwrap(), "a, b");
    }

    #[test]
    fn a_semicolon_header_is_empty_valued() {
        let c = parse("curl 'https://api/x' -H 'X-Empty;'");
        assert_eq!(c.headers.get("X-Empty").unwrap(), "");
    }

    #[test]
    fn a_malformed_header_is_rejected() {
        assert!(err("curl 'https://api/x' -H 'nonsense'").contains("malformed header"));
    }

    #[test]
    fn distinct_query_keys_move_into_the_query_map() {
        let c = parse("curl 'https://api.example.com/x?page=2&limit=10'");
        assert_eq!(c.url, "https://api.example.com/x");
        assert_eq!(c.query.get("page").unwrap(), "2");
        assert_eq!(c.query.get("limit").unwrap(), "10");
    }

    #[test]
    fn query_values_are_decoded() {
        let c = parse("curl 'https://api/x?q=a+b%26c&n=caf%C3%A9'");
        assert_eq!(c.query.get("q").unwrap(), "a b&c");
        assert_eq!(c.query.get("n").unwrap(), "café");
    }

    #[test]
    fn a_repeated_query_key_stays_on_the_url() {
        // Lifting would drop a value, so the whole query stays put.
        let c = parse("curl 'https://api/x?tag=a&tag=b'");
        assert_eq!(c.url, "https://api/x?tag=a&tag=b");
        assert!(c.query.is_empty());
    }

    #[test]
    fn a_placeholder_url_keeps_its_query() {
        let c = parse("curl 'https://{{HOST}}/x?page=2'");
        assert_eq!(c.url, "https://{{HOST}}/x?page=2");
        assert!(c.query.is_empty());
    }

    #[test]
    fn get_flag_folds_data_into_the_query() {
        let c = parse("curl -G 'https://api/x' -d 'page=2'");
        assert_eq!(c.method, "GET");
        assert!(c.body.is_none());
        assert_eq!(c.query.get("page").unwrap(), "2");
    }

    #[test]
    fn data_urlencode_percent_encodes() {
        let c = parse("curl 'https://api/x' --data-urlencode 'q=a b&c'");
        assert_eq!(c.body.expect("body").data, "q=a+b%26c");
    }

    #[test]
    fn data_urlencode_without_a_name_encodes_the_whole_value() {
        let c = parse("curl 'https://api/x' --data-urlencode 'a b'");
        assert_eq!(c.body.expect("body").data, "a+b");
    }

    #[test]
    fn multipart_forms_are_refused() {
        let e = err("curl 'https://api/x' -F 'file=@photo.png'");
        assert!(e.contains("-F"), "error should name the flag: {e}");
    }

    #[test]
    fn file_backed_data_is_refused() {
        let e = err("curl 'https://api/x' -d '@payload.json'");
        assert!(e.contains("payload.json"), "got: {e}");
        // --data-raw exists so that '@' is literal there.
        let c = parse("curl 'https://api/x' --data-raw '@payload.json'");
        assert_eq!(c.body.expect("body").data, "@payload.json");
    }

    #[test]
    fn unrecognized_flags_are_refused() {
        let e = err("curl 'https://api/x' --proxy 'http://p'");
        assert!(e.contains("--proxy"), "error should name the flag: {e}");
    }

    #[test]
    fn an_unsupported_letter_inside_a_bundle_is_caught() {
        // -s and -L are fine; -F is not, and bundling must not hide it.
        let e = err("curl 'https://api/x' -sLF 'file=@x'");
        assert!(e.contains("-F"), "error should name the letter: {e}");
    }

    #[test]
    fn noise_flags_are_ignored() {
        let c = parse("curl -s -v -k -L --compressed -o /dev/null 'https://api/x'");
        assert_eq!(c.url, "https://api/x");
        assert!(c.headers.is_empty(), "got: {:?}", c.headers);
    }

    #[test]
    fn compressed_adds_no_accept_encoding() {
        // reqwest negotiates encoding itself; storing the header would make the
        // config claim something send_request does not do.
        let c = parse("curl --compressed 'https://api/x' -H 'Accept: application/json'");
        assert!(
            !c.headers.keys().any(|k| k.eq_ignore_ascii_case("accept-encoding")),
            "got: {:?}",
            c.headers
        );
    }

    #[test]
    fn a_flag_missing_its_value_is_rejected() {
        assert!(err("curl 'https://api/x' -H").contains("expects a value"));
    }

    #[test]
    fn basic_auth_becomes_an_authorization_header() {
        let c = parse("curl 'https://api/x' -u 'ada:hunter2'");
        assert_eq!(c.headers.get("Authorization").unwrap(), "Basic YWRhOmh1bnRlcjI=");
    }

    #[test]
    fn a_password_less_user_flag_is_refused() {
        let e = err("curl 'https://api/x' -u 'ada'");
        assert!(e.contains("user:password"), "got: {e}");
    }

    #[test]
    fn convenience_flags_become_headers() {
        let c = parse("curl 'https://api/x' -A 'ichigo/1.0' -b 'session=abc' -e 'https://ref'");
        assert_eq!(c.headers.get("User-Agent").unwrap(), "ichigo/1.0");
        assert_eq!(c.headers.get("Cookie").unwrap(), "session=abc");
        assert_eq!(c.headers.get("Referer").unwrap(), "https://ref");
    }

    #[test]
    fn a_cookie_file_is_refused() {
        // `-b file` reads from disk; only an inline cookie is storable.
        assert!(err("curl 'https://api/x' -b 'cookies.txt'").contains("cookie file"));
    }

    #[test]
    fn base64_covers_every_remainder() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64_encode("café:pw".as_bytes()), "Y2Fmw6k6cHc=");
    }

    #[test]
    fn a_devtools_style_command_parses() {
        // The shape Chrome's "Copy as cURL" produces: noise flags, many
        // headers, continuations, and an ANSI-C quoted body.
        let cmd = "curl 'https://api.example.com/v1/items?page=2' \\\n  \
             -H 'authority: api.example.com' \\\n  \
             -H 'accept: application/json' \\\n  \
             -H 'content-type: application/json' \\\n  \
             --data-raw $'{\\n  \"name\": \"caf\\xc3\\xa9\"\\n}' \\\n  \
             --compressed";
        let c = parse(cmd);
        assert_eq!(c.method, "POST");
        assert_eq!(c.url, "https://api.example.com/v1/items");
        assert_eq!(c.query.get("page").unwrap(), "2");
        assert_eq!(c.headers.get("accept").unwrap(), "application/json");
        let body = c.body.expect("body");
        assert_eq!(body.content_type, "application/json");
        assert_eq!(body.data, "{\n  \"name\": \"café\"\n}");
    }

    // ─── Round trip ──────────────────────────────────────────────────────────

    #[test]
    fn a_rendered_request_parses_back_to_itself() {
        let mut c = req("POST", "https://api.example.com/users");
        c.headers.insert("Accept".to_string(), "application/json".to_string());
        c.headers.insert("X-Note".to_string(), "it's here".to_string());
        c.query.insert("page".to_string(), "2".to_string());
        c.query.insert("q".to_string(), "a b&c".to_string());
        c.body = Some(Body {
            content_type: "application/json".to_string(),
            data: "{\n  \"name\": \"ada\"\n}".to_string(),
        });

        let back = from_curl(&to_curl(&c, &HashMap::new())).expect("round trips");

        assert_eq!(back.method, c.method);
        assert_eq!(back.url, c.url);
        assert_eq!(back.headers, c.headers);
        assert_eq!(back.query, c.query);
        let (a, b) = (back.body.expect("body"), c.body.expect("body"));
        assert_eq!(a.content_type, b.content_type);
        assert_eq!(a.data, b.data);
    }

    #[test]
    fn a_rendered_get_parses_back_to_itself() {
        let mut c = req("GET", "https://api.example.com/items");
        c.headers.insert("Authorization".to_string(), "Bearer abc123".to_string());
        let back = from_curl(&to_curl(&c, &HashMap::new())).expect("round trips");
        assert_eq!(back.method, "GET");
        assert_eq!(back.url, c.url);
        assert_eq!(back.headers, c.headers);
        assert!(back.body.is_none());
    }

    #[test]
    fn round_trip_substitutes_resolved_variables() {
        let mut c = req("GET", "https://{{HOST}}/x");
        c.headers.insert("Authorization".to_string(), "Bearer {{TOKEN}}".to_string());
        let rendered = to_curl(&c, &vars(&[("HOST", "api.test"), ("TOKEN", "abc123")]));
        let back = from_curl(&rendered).expect("round trips");
        assert_eq!(back.url, "https://api.test/x");
        assert_eq!(back.headers.get("Authorization").unwrap(), "Bearer abc123");
    }

    #[test]
    fn fields_a_command_cannot_express_come_back_empty() {
        let mut c = req("GET", "https://api.example.com/x");
        c.description = Some("a description".to_string());
        c.profiles = Some(vec![crate::config::Profile {
            name: "dev".to_string(),
            params: HashMap::new(),
        }]);
        c.extract = Some(HashMap::new());

        let back = from_curl(&to_curl(&c, &HashMap::new())).expect("round trips");
        assert!(back.name.is_empty(), "the caller supplies the name");
        assert!(back.description.is_none());
        assert!(back.profiles.is_none());
        assert!(back.extract.is_none());
    }
}
