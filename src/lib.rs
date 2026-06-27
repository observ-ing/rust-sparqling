//! Minimal async SPARQL-over-HTTP client.
//!
//! Sends queries to a SPARQL endpoint over HTTP POST and parses the standard
//! SPARQL 1.1 JSON results format. Endpoint-agnostic: point it at Wikidata, a
//! local Oxigraph, or any other SPARQL service.
//!
//! # Example
//!
//! ```no_run
//! use sparql_client::SparqlClient;
//!
//! # async fn example() {
//! let client = SparqlClient::new("https://query.wikidata.org/sparql");
//!
//! let rows = client
//!     .sparql_query("SELECT ?item WHERE { ?item wdt:P31 wd:Q5 } LIMIT 5")
//!     .await
//!     .unwrap();
//! # }
//! ```

use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::{Client, StatusCode};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;

const DEFAULT_USER_AGENT: &str = "sparql-client/0.1 (Rust)";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// A single value (RDF term) returned in a SPARQL result binding.
#[derive(Debug, Clone, Deserialize)]
pub struct SparqlValue {
    /// `"uri"`, `"literal"`, `"bnode"`, or `"typed-literal"`.
    #[serde(rename = "type")]
    pub value_type: Option<String>,
    pub value: String,
    /// Datatype IRI for typed literals, e.g. `http://www.w3.org/2001/XMLSchema#integer`.
    pub datatype: Option<String>,
    #[serde(rename = "xml:lang")]
    pub lang: Option<String>,
}

impl SparqlValue {
    /// True if this term is an IRI (`type` is `"uri"`).
    pub fn is_uri(&self) -> bool {
        self.value_type.as_deref() == Some("uri")
    }

    /// True if this term is a literal — plain, language-tagged, or typed.
    pub fn is_literal(&self) -> bool {
        matches!(
            self.value_type.as_deref(),
            Some("literal") | Some("typed-literal")
        )
    }

    /// True if this term is a blank node (`type` is `"bnode"`).
    pub fn is_bnode(&self) -> bool {
        self.value_type.as_deref() == Some("bnode")
    }

    /// The local datatype name, e.g. `"integer"` for
    /// `http://www.w3.org/2001/XMLSchema#integer`. `None` for non-typed terms.
    pub fn datatype_name(&self) -> Option<&str> {
        let dt = self.datatype.as_deref()?;
        Some(dt.rsplit(['#', '/']).next().unwrap_or(dt))
    }

    /// Parse the value as an `i64` (`xsd:integer` and friends).
    pub fn as_i64(&self) -> Option<i64> {
        self.value.trim().parse().ok()
    }

    /// Parse the value as an `f64` (`xsd:decimal`, `xsd:double`, `xsd:float`).
    pub fn as_f64(&self) -> Option<f64> {
        self.value.trim().parse().ok()
    }

    /// Parse the value as an `xsd:boolean` — accepts `true`/`false` and `1`/`0`.
    pub fn as_bool(&self) -> Option<bool> {
        match self.value.trim() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        }
    }

    /// Parse the value as an RFC 3339 / `xsd:dateTime` timestamp.
    ///
    /// Requires the `chrono` feature.
    #[cfg(feature = "chrono")]
    pub fn as_datetime(&self) -> Option<chrono::DateTime<chrono::FixedOffset>> {
        chrono::DateTime::parse_from_rfc3339(self.value.trim()).ok()
    }
}

/// A row of results from a SPARQL query, mapping variable names to values.
pub type SparqlBinding = HashMap<String, SparqlValue>;

/// Deserialize a single result row into `T`.
///
/// Each term is coerced to a JSON scalar based on its `xsd:` datatype
/// (integers → numbers, `xsd:boolean` → bool, decimals/doubles → floats),
/// then handed to `T`'s `Deserialize` impl. IRIs and untyped literals stay
/// strings. This is the per-row primitive behind
/// [`SparqlClient::query_into`].
pub fn from_binding<T: DeserializeOwned>(binding: &SparqlBinding) -> Result<T, Error> {
    let map: serde_json::Map<String, serde_json::Value> = binding
        .iter()
        .map(|(var, value)| (var.clone(), value_to_json(value)))
        .collect();
    serde_json::from_value(serde_json::Value::Object(map)).map_err(Error::Deserialize)
}

/// Coerce a SPARQL term to a JSON scalar using its datatype IRI, so serde can
/// deserialize numeric / boolean columns without the caller writing custom
/// `deserialize_with` adapters. Anything unrecognized stays a string.
fn value_to_json(value: &SparqlValue) -> serde_json::Value {
    use serde_json::Value as Json;

    // IRIs and blank nodes are always strings.
    if value.value_type.as_deref() != Some("literal")
        && value.value_type.as_deref() != Some("typed-literal")
    {
        return Json::String(value.value.clone());
    }

    let as_string = || Json::String(value.value.clone());
    let trimmed = value.value.trim();
    match value.datatype_name() {
        Some(
            "integer" | "int" | "long" | "short" | "byte" | "nonNegativeInteger"
            | "positiveInteger" | "negativeInteger" | "nonPositiveInteger" | "unsignedInt"
            | "unsignedLong" | "unsignedShort" | "unsignedByte",
        ) => trimmed
            .parse::<i64>()
            .map(|n| Json::Number(n.into()))
            .unwrap_or_else(|_| as_string()),
        Some("decimal" | "double" | "float") => trimmed
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Json::Number)
            .unwrap_or_else(as_string),
        Some("boolean") => match trimmed {
            "true" | "1" => Json::Bool(true),
            "false" | "0" => Json::Bool(false),
            _ => as_string(),
        },
        _ => as_string(),
    }
}

/// Full SPARQL JSON results — handles SELECT (`results.bindings`) and ASK (`boolean`).
#[derive(Debug, Default, Deserialize)]
struct SparqlResponse {
    #[serde(default)]
    results: SparqlResults,
    /// Present only for ASK queries.
    boolean: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct SparqlResults {
    #[serde(default)]
    bindings: Vec<SparqlBinding>,
}

/// Async client for a single SPARQL endpoint.
pub struct SparqlClient {
    client: Client,
    endpoint: String,
}

impl SparqlClient {
    /// Create a client for `endpoint` with a default user agent.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self::with_user_agent(endpoint, DEFAULT_USER_AGENT)
    }

    /// Create a client for `endpoint` with a custom user agent.
    ///
    /// Many public endpoints (Wikidata in particular) require a meaningful
    /// user agent — requests with generic agents may be throttled or blocked.
    ///
    /// Panics if the underlying HTTP client cannot be built; use
    /// [`try_with_user_agent`](Self::try_with_user_agent) to handle that case.
    pub fn with_user_agent(endpoint: impl Into<String>, user_agent: &str) -> Self {
        Self::try_with_user_agent(endpoint, user_agent).expect("reqwest client should build")
    }

    /// Fallible constructor that surfaces HTTP-client build errors instead of
    /// silently falling back to a default client.
    pub fn try_with_user_agent(
        endpoint: impl Into<String>,
        user_agent: &str,
    ) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: Client::builder()
                .user_agent(user_agent)
                .timeout(DEFAULT_TIMEOUT)
                .build()?,
            endpoint: endpoint.into(),
        })
    }

    /// The endpoint URL this client targets.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Execute a SELECT-style SPARQL query and return the result bindings.
    pub async fn sparql_query(&self, query: &str) -> Result<Vec<SparqlBinding>, Error> {
        Ok(self.run(query).await?.results.bindings)
    }

    /// Execute a SELECT-style query and deserialize each row into `T`.
    ///
    /// Variable names map to struct fields; literal terms are coerced to
    /// numbers / booleans according to their `xsd:` datatype, and everything
    /// else (IRIs, plain literals) maps to a `String`. Unbound optional
    /// variables map cleanly to `Option<T>` fields.
    ///
    /// ```no_run
    /// # use serde::Deserialize;
    /// # use sparql_client::SparqlClient;
    /// #[derive(Deserialize)]
    /// struct Person {
    ///     item: String,
    ///     count: i64,
    /// }
    ///
    /// # async fn example() -> Result<(), sparql_client::Error> {
    /// let client = SparqlClient::new("https://query.wikidata.org/sparql");
    /// let people: Vec<Person> = client.query_into("SELECT ?item ?count WHERE { … }").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn query_into<T: DeserializeOwned>(&self, query: &str) -> Result<Vec<T>, Error> {
        self.sparql_query(query)
            .await?
            .iter()
            .map(from_binding)
            .collect()
    }

    /// Execute an `ASK { … }` query.
    pub async fn sparql_ask(&self, query: &str) -> Result<bool, Error> {
        self.run(query).await?.boolean.ok_or(Error::UnexpectedShape)
    }

    /// Send a query to the endpoint and parse the SPARQL JSON response.
    async fn run(&self, query: &str) -> Result<SparqlResponse, Error> {
        let response = self
            .client
            .post(&self.endpoint)
            // Send the query in the body so long queries don't hit URL-length limits.
            .header(CONTENT_TYPE, "application/sparql-query")
            .header(ACCEPT, "application/sparql-results+json")
            .body(query.to_string())
            .send()
            .await
            .map_err(Error::Transport)?;

        let status = response.status();
        if !status.is_success() {
            // Endpoints report query-timeout / syntax errors in the body — keep a snippet.
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Status {
                status,
                body: truncate(&body, 512),
            });
        }

        response
            .json::<SparqlResponse>()
            .await
            .map_err(Error::Decode)
    }
}

/// Escape a string for use inside a SPARQL double-quoted literal.
///
/// Without this, a value containing `"`, `\`, or a newline breaks the query
/// (or allows injection). See the SPARQL 1.1 `STRING_LITERAL` grammar.
///
/// ```
/// use sparql_client::escape_literal;
///
/// assert_eq!(escape_literal(r#"a "b" \ c"#), r#"a \"b\" \\ c"#);
/// ```
pub fn escape_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str(r"\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            '\t' => out.push_str(r"\t"),
            _ => out.push(c),
        }
    }
    out
}

/// Truncate a string to at most `max` bytes, appending `…` if it was cut.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// Errors that can occur when querying a SPARQL endpoint.
#[derive(Debug)]
pub enum Error {
    /// The request never completed (DNS, TLS, connect, or timeout).
    Transport(reqwest::Error),
    /// The endpoint returned a non-success status. `body` is a truncated
    /// snippet of the response — endpoints report query timeouts and syntax
    /// errors there.
    Status { status: StatusCode, body: String },
    /// The response could not be decoded as SPARQL JSON.
    Decode(reqwest::Error),
    /// A result row could not be deserialized into the requested type (see
    /// [`SparqlClient::query_into`]).
    Deserialize(serde_json::Error),
    /// The response was valid JSON but not the expected shape (e.g. an ASK
    /// query returned no `boolean`).
    UnexpectedShape,
}

impl Error {
    /// HTTP 429 / 503 — the caller may retry with backoff.
    pub fn is_throttled(&self) -> bool {
        matches!(self, Error::Status { status, .. }
            if *status == StatusCode::TOO_MANY_REQUESTS
                || *status == StatusCode::SERVICE_UNAVAILABLE)
    }

    /// The request timed out.
    pub fn is_timeout(&self) -> bool {
        matches!(self, Error::Transport(e) if e.is_timeout())
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Transport(e) => write!(f, "SPARQL request error: {e}"),
            Error::Status { status, body } => write!(f, "SPARQL HTTP error: {status}: {body}"),
            Error::Decode(e) => write!(f, "SPARQL response decode error: {e}"),
            Error::Deserialize(e) => write!(f, "SPARQL row deserialize error: {e}"),
            Error::UnexpectedShape => write!(f, "SPARQL response had an unexpected shape"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Transport(e) | Error::Decode(e) => Some(e),
            Error::Deserialize(e) => Some(e),
            Error::Status { .. } | Error::UnexpectedShape => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_literal() {
        assert_eq!(escape_literal("plain"), "plain");
        assert_eq!(escape_literal(r#"say "hi""#), r#"say \"hi\""#);
        assert_eq!(escape_literal(r"back\slash"), r"back\\slash");
        assert_eq!(escape_literal("line\nbreak"), r"line\nbreak");
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("abcdef", 3), "abc…");
        // Does not split a multi-byte char.
        assert_eq!(truncate("aé", 2), "a…");
    }

    fn typed(value: &str, datatype: &str) -> SparqlValue {
        SparqlValue {
            value_type: Some("typed-literal".into()),
            value: value.into(),
            datatype: Some(format!("http://www.w3.org/2001/XMLSchema#{datatype}")),
            lang: None,
        }
    }

    fn uri(value: &str) -> SparqlValue {
        SparqlValue {
            value_type: Some("uri".into()),
            value: value.into(),
            datatype: None,
            lang: None,
        }
    }

    #[test]
    fn test_term_kind() {
        let u = uri("http://example.com/x");
        assert!(u.is_uri() && !u.is_literal() && !u.is_bnode());
        let l = typed("1", "integer");
        assert!(l.is_literal() && !l.is_uri());
    }

    #[test]
    fn test_value_parsing() {
        assert_eq!(typed("42", "integer").as_i64(), Some(42));
        assert_eq!(typed(" 42 ", "integer").as_i64(), Some(42));
        assert_eq!(typed("3.5", "decimal").as_f64(), Some(3.5));
        assert_eq!(typed("true", "boolean").as_bool(), Some(true));
        assert_eq!(typed("0", "boolean").as_bool(), Some(false));
        assert_eq!(typed("nope", "boolean").as_bool(), None);
        assert_eq!(typed("x", "integer").as_i64(), None);
    }

    #[test]
    fn test_datatype_name() {
        assert_eq!(typed("1", "integer").datatype_name(), Some("integer"));
        assert_eq!(uri("http://example.com/x").datatype_name(), None);
    }

    #[cfg(feature = "chrono")]
    #[test]
    fn test_as_datetime() {
        let v = typed("2024-01-02T03:04:05Z", "dateTime");
        let dt = v.as_datetime().unwrap();
        assert_eq!(dt.to_rfc3339(), "2024-01-02T03:04:05+00:00");
        assert!(uri("http://example.com/x").as_datetime().is_none());
    }

    #[test]
    fn test_from_binding_coerces_types() {
        #[derive(Debug, PartialEq, serde::Deserialize)]
        struct Row {
            item: String,
            count: i64,
            ratio: f64,
            active: bool,
            label: Option<String>,
        }

        let mut binding = SparqlBinding::new();
        binding.insert("item".into(), uri("http://example.com/x"));
        binding.insert("count".into(), typed("42", "integer"));
        binding.insert("ratio".into(), typed("3.5", "decimal"));
        binding.insert("active".into(), typed("true", "boolean"));

        let row: Row = from_binding(&binding).unwrap();
        assert_eq!(
            row,
            Row {
                item: "http://example.com/x".into(),
                count: 42,
                ratio: 3.5,
                active: true,
                label: None, // unbound optional variable
            }
        );
    }

    #[test]
    fn test_from_binding_reports_type_mismatch() {
        #[derive(Debug, serde::Deserialize)]
        struct Row {
            #[allow(dead_code)]
            count: i64,
        }

        let mut binding = SparqlBinding::new();
        // A non-numeric value where i64 is expected surfaces as Error::Deserialize.
        binding.insert("count".into(), typed("not-a-number", "string"));
        assert!(matches!(
            from_binding::<Row>(&binding),
            Err(Error::Deserialize(_))
        ));
    }
}
