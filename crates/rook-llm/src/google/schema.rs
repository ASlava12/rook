//! Gemini's string/number const compatibility, confined to schema positions.
use serde_json::Value;

pub(super) fn parameters(input: &Value) -> Value {
    let mut schema = input.clone();
    rewrite(&mut schema);
    schema
}

fn rewrite(schema: &mut Value) {
    match schema {
        Value::Array(schemas) => schemas.iter_mut().for_each(rewrite),
        Value::Object(fields) => {
            // Keep an explicitly supplied enum and all other const value types.
            if !fields.contains_key("enum")
                && let Some(value) = fields.get("const").filter(|v| v.is_string() || v.is_number()).cloned()
            {
                fields.remove("const");
                fields.insert("enum".into(), Value::Array(vec![value]));
            }
            for (keyword, value) in fields {
                match keyword.as_str() {
                    "properties" | "patternProperties" | "$defs" | "definitions" | "dependentSchemas"
                    | "dependencies" => {
                        // These keys are property/definition names, never keywords.
                        if let Value::Object(named) = value {
                            named.values_mut().for_each(rewrite);
                        }
                    }
                    "additionalProperties"
                    | "unevaluatedProperties"
                    | "propertyNames"
                    | "items"
                    | "prefixItems"
                    | "additionalItems"
                    | "unevaluatedItems"
                    | "contains"
                    | "allOf"
                    | "anyOf"
                    | "oneOf"
                    | "not"
                    | "if"
                    | "then"
                    | "else"
                    | "contentSchema" => rewrite(value),
                    // Defaults, examples, enum/const values and unknown annotations
                    // are literal data, even if they look like another schema.
                    _ => {}
                }
            }
        }
        _ => {}
    }
}
