# sparql-client

Minimal async [SPARQL](https://www.w3.org/TR/sparql11-query/)-over-HTTP client for Rust.

Sends queries to a SPARQL endpoint over HTTP POST and parses the standard
SPARQL 1.1 JSON results format. Endpoint-agnostic: point it at
[Wikidata](https://query.wikidata.org/sparql), a local
[Oxigraph](https://github.com/oxigraph/oxigraph), or any other SPARQL service.

## Usage

Add it to your `Cargo.toml`:

```toml
[dependencies]
sparql-client = { git = "https://github.com/observ-ing/sparql-client" }
```

```rust
use sparql_client::SparqlClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = SparqlClient::new("https://query.wikidata.org/sparql");

    let rows = client
        .sparql_query("SELECT ?item WHERE { ?item wdt:P31 wd:Q5 } LIMIT 5")
        .await?;

    for row in rows {
        if let Some(item) = row.get("item") {
            println!("{}", item.value);
        }
    }

    Ok(())
}
```

## Features

- **SELECT** queries via [`SparqlClient::sparql_query`], returning result bindings.
- **ASK** queries via [`SparqlClient::sparql_ask`], returning a `bool`.
- Queries are sent in the request body, so long queries don't hit URL-length limits.
- A custom user agent can be set with [`SparqlClient::with_user_agent`] — many
  public endpoints (Wikidata in particular) require a meaningful user agent.
- [`escape_literal`] for safely embedding strings in double-quoted SPARQL literals.
- Rich [`Error`] type with [`Error::is_throttled`] (HTTP 429 / 503) and
  [`Error::is_timeout`] helpers for retry logic.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
