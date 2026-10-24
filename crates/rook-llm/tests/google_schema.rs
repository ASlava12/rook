//! Arbitrary tool JSON Schema must use the API's JSON Schema declaration field.
use futures_util::StreamExt;
use rook_llm::google::{Config, Google};
use rook_llm::{Message, Provider, Request, ToolSpec};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn capture(stream: bool, schemas: &[Value]) -> Value {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let body_start = loop {
            let mut block = [0u8; 2048];
            let n = socket.read(&mut block).await.unwrap();
            assert_ne!(n, 0, "request ended before its headers");
            bytes.extend_from_slice(&block[..n]);
            assert!(bytes.len() < 65536);
            if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let headers = std::str::from_utf8(&bytes[..body_start]).unwrap();
        let length = headers
            .lines()
            .find_map(|line| line.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("content-length")))
            .unwrap()
            .1
            .trim()
            .parse::<usize>()
            .unwrap();
        assert!(length < 32768);
        assert!(headers.starts_with(if stream {
            "POST /v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse "
        } else {
            "POST /v1beta/models/gemini-2.5-pro:generateContent "
        }));
        while bytes.len() < body_start + length {
            let mut block = [0u8; 2048];
            let n = socket.read(&mut block).await.unwrap();
            assert_ne!(n, 0, "request ended before its declared body");
            bytes.extend_from_slice(&block[..n]);
            assert!(bytes.len() < 65536);
        }
        let request: Value = serde_json::from_slice(&bytes[body_start..body_start + length]).unwrap();
        let answer =
            r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]},"finishReason":"STOP"}]}"#;
        let (kind, reply) = if stream {
            ("text/event-stream", format!("data: {answer}\n\n"))
        } else {
            ("application/json", answer.to_owned())
        };
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{reply}",
            reply.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        request
    });
    let config =
        Config::new(format!("http://{address}/v1beta"), "artificial-test-key".into(), "gemini-2.5-pro");
    let provider = Google::new("owned-google-fixture", "gemini-2.5-pro", config).unwrap();
    let mut request = Request::new(vec![Message::user("go")]);
    request.tools = schemas
        .iter()
        .enumerate()
        .map(|(index, schema)| ToolSpec {
            name: format!("tool_{index}"),
            description: format!("description {index}"),
            parameters: schema.clone(),
        })
        .collect();
    if stream {
        let mut response = provider.stream(request).await.unwrap();
        while let Some(delta) = response.next().await {
            delta.unwrap();
        }
    } else {
        assert_eq!(provider.complete(request).await.unwrap().message.content, "ok");
    }
    server.await.unwrap()
}

async fn declaration(stream: bool) {
    let schemas = [
        json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}),
        json!({"type":"object","$defs":{"choice":{"oneOf":[{"type":"string"},{"type":"integer"}]}},
               "properties":{"value":{"$ref":"#/$defs/choice"},"const":{"type":"object",
                 "default":{"const":"literal","properties":{"nested":{"const":7}}},
                 "examples":[{"const":"example"}],"x-custom":{"const":"annotation"}}},
               "required":["value"],"additionalProperties":false}),
    ];
    let wire = capture(stream, &schemas).await;
    let tools = wire["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    let declarations = tools[0]["functionDeclarations"].as_array().unwrap();
    assert_eq!(declarations.len(), schemas.len());
    for (index, (declaration, schema)) in declarations.iter().zip(schemas).enumerate() {
        assert_eq!(declaration["name"], format!("tool_{index}"));
        assert_eq!(declaration["description"], format!("description {index}"));
        assert_eq!(
            declaration.get("parametersJsonSchema"),
            Some(&schema),
            "arbitrary JSON Schema belongs in parametersJsonSchema"
        );
        assert!(declaration.get("parameters").is_none(), "the API fields are mutually exclusive");
    }
}

#[tokio::test]
async fn complete_tool_schemas_use_the_json_schema_field() {
    tokio::time::timeout(std::time::Duration::from_secs(10), declaration(false)).await.unwrap();
}

#[tokio::test]
async fn streamed_tool_schemas_use_the_json_schema_field() {
    tokio::time::timeout(std::time::Duration::from_secs(10), declaration(true)).await.unwrap();
}

async fn scalar_constraints(stream: bool) {
    let input = json!({"type":"object","properties":{
        "name":{"type":"string","const":"fixed"},
        "count":{"type":"integer","const":7},
        "ratio":{"type":"number","const":1.5},
        "choice":{"oneOf":[{"const":"a"},{"const":2}]},
        "sequence":{"type":"array","items":{"const":"item"}},
        "const":{"const":"property-named-const"},
        "properties":{"const":"property-named-properties"}
    },"required":["name"],"additionalProperties":false});
    let expected = json!({"type":"object","properties":{
        "name":{"type":"string","enum":["fixed"]},
        "count":{"type":"integer","enum":[7]},
        "ratio":{"type":"number","enum":[1.5]},
        "choice":{"oneOf":[{"enum":["a"]},{"enum":[2]}]},
        "sequence":{"type":"array","items":{"enum":["item"]}},
        "const":{"enum":["property-named-const"]},
        "properties":{"enum":["property-named-properties"]}
    },"required":["name"],"additionalProperties":false});
    let wire = capture(stream, std::slice::from_ref(&input)).await;
    assert_eq!(wire["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"], expected);
    assert_eq!(input["properties"]["name"]["const"], "fixed", "conversion leaves its input intact");
}

#[tokio::test]
async fn complete_scalar_consts_use_singleton_enums_in_schema_positions() {
    tokio::time::timeout(std::time::Duration::from_secs(10), scalar_constraints(false)).await.unwrap();
}

#[tokio::test]
async fn streamed_scalar_consts_use_singleton_enums_in_schema_positions() {
    tokio::time::timeout(std::time::Duration::from_secs(10), scalar_constraints(true)).await.unwrap();
}

#[tokio::test]
async fn schema_maps_subschemas_and_literal_values_remain_distinct_on_the_wire() {
    let mut cases = Vec::new();
    for keyword in
        ["properties", "patternProperties", "$defs", "definitions", "dependentSchemas", "dependencies"]
    {
        let mut input = json!({"type":"object"});
        input[keyword] = json!({"__proto__":{"const":"x"},"const":{"const":3},"properties":{"const":"z"}});
        let mut expected = json!({"type":"object"});
        expected[keyword] =
            json!({"__proto__":{"enum":["x"]},"const":{"enum":[3]},"properties":{"enum":["z"]}});
        cases.push((input, expected));
    }
    for keyword in [
        "additionalProperties",
        "unevaluatedProperties",
        "propertyNames",
        "items",
        "additionalItems",
        "unevaluatedItems",
        "contains",
        "not",
        "if",
        "then",
        "else",
        "contentSchema",
    ] {
        let mut input = json!({"type":"object"});
        input[keyword] = json!({"const":"fixed"});
        let mut expected = json!({"type":"object"});
        expected[keyword] = json!({"enum":["fixed"]});
        cases.push((input, expected));
    }
    for keyword in ["allOf", "anyOf", "oneOf", "prefixItems", "items"] {
        let mut input = json!({"type":"object"});
        input[keyword] = json!([{"const":-1},{"const":"second"}]);
        let mut expected = json!({"type":"object"});
        expected[keyword] = json!([{"enum":[-1]},{"enum":["second"]}]);
        cases.push((input, expected));
    }
    for keyword in ["default", "examples", "enum", "const", "x-custom"] {
        let literal = json!({"const":"literal","properties":{"nested":{"const":1}}});
        let mut input = json!({"type":"object"});
        input[keyword] = if matches!(keyword, "examples" | "enum") { json!([literal]) } else { literal };
        cases.push((input.clone(), input));
    }
    for value in [json!(true), json!(false), json!(null), json!(["array"]), json!({"const":"object"})] {
        let input = json!({"type":"object","properties":{"value":{"const":value}}});
        cases.push((input.clone(), input));
    }
    let explicit_enum = json!({"type":"object","properties":{"value":{"const":"a","enum":["a","b"]}}});
    cases.push((explicit_enum.clone(), explicit_enum));
    let flags = json!({"type":"object","properties":{"yes":true,"no":false},"dependencies":{"yes":["no"]}});
    cases.push((flags.clone(), flags));
    let original: Vec<Value> = cases.iter().map(|(input, _)| input.clone()).collect();
    for stream in [false, true] {
        let wire = tokio::time::timeout(std::time::Duration::from_secs(10), capture(stream, &original))
            .await
            .unwrap();
        let declarations = wire["tools"][0]["functionDeclarations"].as_array().unwrap();
        assert_eq!(declarations.len(), cases.len());
        for (index, (declaration, (_, expected))) in declarations.iter().zip(&cases).enumerate() {
            assert_eq!(&declaration["parametersJsonSchema"], expected, "case {index}, stream={stream}");
            assert!(declaration.get("parameters").is_none());
        }
    }
    assert_eq!(original, cases.iter().map(|(input, _)| input.clone()).collect::<Vec<_>>());
}
