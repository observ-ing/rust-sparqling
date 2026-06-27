# sparql-client

Minimal async [SPARQL](https://www.w3.org/TR/sparql11-query/)-over-HTTP client for Rust.

Sends queries to a SPARQL endpoint over HTTP POST and parses the SPARQL 1.1
JSON results format. Point it at [Wikidata](https://query.wikidata.org/sparql),
a local [Oxigraph](https://github.com/oxigraph/oxigraph), or any other endpoint.

```toml
[dependencies]
sparql-http-client = "0.1"
```

```rust
use sparql_client::SparqlClient;

let client = SparqlClient::new("https://query.wikidata.org/sparql");
let rows = client
    .sparql_query("SELECT ?item WHERE { ?item wdt:P31 wd:Q5 } LIMIT 5")
    .await?;
```

## Features

- **SELECT** (`sparql_query`) and **ASK** (`sparql_ask`) queries.
- **Typed rows** — `query_into::<T>()` deserializes each binding into your own
  struct, coercing `xsd:` datatypes to numbers/booleans.
- **Typed accessors** on `SparqlValue` (`as_i64`, `as_bool`, `is_uri`, …;
  `as_datetime` behind the `chrono` feature).
- **Configurable client** via `SparqlClient::builder()` — user agent, timeout,
  or a shared `reqwest::Client`.
- **Retries** with exponential backoff that honor `Retry-After`
  (`.max_retries(n)`).
- `escape_literal` for safely embedding strings in SPARQL literals.

Queries are sent in the request body, so long queries don't hit URL-length
limits. Many public endpoints (Wikidata especially) require a meaningful user
agent — set one with the builder or `with_user_agent`.

## License

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT) at
your option.
