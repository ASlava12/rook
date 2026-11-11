//! The network API can touch only the task owned by its authenticated route.
use super::*;
use crate::remote_agents::{DOMAIN, Envelope, Operation, signature};

fn unauth(message: &str) -> CoreError {
    CoreError::InboundAuthentication(message.into())
}

impl Rook {
    /// Authenticate admission and sign the exact bounded response bytes.
    pub fn agent_http_reply(&self, request: Delivery<'_>, vault: &crate::Vault) -> Result<(Vec<u8>, String)> {
        let value = self.receive_agent_request(request, vault)?;
        let bytes = crate::persistence::encode_with_limit(&value, self.config.remote_agents.response_bytes)?;
        let handler = self
            .config
            .inbound
            .routes
            .get(request.route)
            .ok_or_else(|| unauth("remote route disappeared"))?;
        let secret =
            vault.value(&handler.secret).ok_or_else(|| unauth("remote route credential unavailable"))?;
        use hmac::Mac;
        let mac = crate::remote_agents::reply_mac(
            request.route,
            request.id,
            request.timestamp,
            ObjectId::of(request.body).as_bytes(),
            &bytes,
            secret.as_bytes(),
        )?;
        let code = format!("v1={}", hex::encode(mac.finalize().into_bytes()));
        Ok((bytes, code))
    }
    /// Dedicated HMAC domain separates peer requests from ordinary event delivery.
    pub fn receive_agent_request(&self, request: Delivery<'_>, vault: &crate::Vault) -> Result<Value> {
        let Delivery { route, id: delivery, timestamp, signature: signed, body } = request;
        if !self.config.remote_agents.enabled
            || !self.config.remote_agents.routes.contains(route)
            || !self.config.remote_agents.errors().is_empty()
            || !self.config.inbound.enabled
            || !self.config.inbound.errors().is_empty()
        {
            return Err(unauth("remote agent ingress is disabled or invalid"));
        }
        let handler =
            self.config.inbound.routes.get(route).ok_or_else(|| unauth("unknown remote agent route"))?;
        let limit = self.config.inbound.body_bytes.min(self.config.remote_agents.request_bytes);
        if body.len() > limit {
            return Err(bad("remote request exceeds configured body limit"));
        }
        auth::headers(route, delivery, timestamp, signed, body.len(), &self.config.inbound, managed::now())?;
        let secret =
            vault.value(&handler.secret).ok_or_else(|| unauth("remote route credential is unavailable"))?;
        // Mac::verify_slice compares in constant time; never compare signature strings.
        use hmac::{Hmac, Mac};
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes())
            .map_err(|_| unauth("invalid remote credential"))?;
        if !(16..=4096).contains(&secret.len()) {
            return Err(unauth("invalid remote credential length"));
        }
        mac.update(DOMAIN);
        for field in [route, delivery, timestamp] {
            mac.update(field.as_bytes());
            mac.update(b"\0");
        }
        mac.update(body);
        let code = hex::decode(&signed[3..]).map_err(|_| unauth("invalid remote signature"))?;
        mac.verify_slice(&code).map_err(|_| unauth("remote authentication failed"))?;
        let mut envelope: Envelope =
            serde_json::from_slice(body).map_err(|_| bad("invalid remote request envelope"))?;
        if envelope.version != 1 || rook_store::parse_session_id(&envelope.sender).is_none() {
            return Err(bad("unsupported remote protocol version or invalid sender session"));
        }
        envelope.operation.validate()?;
        if let Operation::Submit { text } | Operation::Message { text, .. } = &mut envelope.operation {
            *text = vault.redact(text);
        }
        let mut response = match &envelope.operation {
            Operation::Info => json!({"protocol":"rook-a2a-v1","route":route,"task":handler.task,
                "workspace":handler.workspace,"autonomous":handler.autonomous,
                "limits":{"max_iterations":handler.max_iterations,"max_tokens":handler.max_tokens,"max_seconds":handler.max_seconds},
                "capabilities":["submit","status","message","result","cancel"],
                "max_text_bytes":16384,"request_bytes":limit}),
            Operation::Submit { .. } => {
                // Reuse the existing atomic source+receipt admission and ordinary
                // supervisor. The payload cannot choose instructions or workspace.
                let redacted = crate::persistence::encode_with_limit(
                    &json!({"protocol":"rook-a2a-v1",
                    "sender_session":envelope.sender,"operation":envelope.operation,"wire_hash":ObjectId::of(body).to_hex()}),
                    self.config.inbound.body_bytes,
                )?;
                let local = signature(
                    b"rook-inbound-v1\0",
                    route,
                    delivery,
                    timestamp,
                    &redacted,
                    secret.as_bytes(),
                )?;
                self.receive_event(
                    Delivery { route, id: delivery, timestamp, signature: &local, body: &redacted },
                    vault,
                )?
            }
            operation => self.scoped_agent_operation(route, delivery, &envelope.sender, operation, vault)?,
        };
        redact_response(&mut response, vault);
        let response = json!({"protocol":"rook-a2a-v1","result":response});
        crate::persistence::encode_with_limit(&response, self.config.remote_agents.response_bytes)?;
        Ok(response)
    }

    fn scoped_agent_operation(
        &self,
        route: &str,
        delivery: &str,
        sender: &str,
        operation: &Operation,
        vault: &crate::Vault,
    ) -> Result<Value> {
        let id = match operation {
            Operation::Status { id }
            | Operation::Message { id, .. }
            | Operation::Result { id, .. }
            | Operation::Cancel { id } => id,
            _ => return Err(bad("invalid scoped remote operation")),
        };
        let (_, book) = read(self)?;
        let receipt = book
            .receipts
            .iter()
            .find(|receipt| receipt.route == route && receipt.id == *id)
            .ok_or_else(|| bad("remote task receipt is not owned by this route"))?;
        if matches!(operation, Operation::Status { .. }) {
            let mut result = view(self, receipt)?;
            if let Some(identity) = &receipt.run {
                let saved = managed::read(self, &identity.id)?;
                if saved.run.identity() == *identity
                    && let Some(work) = result.get_mut("work").and_then(Value::as_object_mut)
                {
                    for (key, text) in
                        [("reply", &saved.run.reply), ("verification", &saved.run.verification)]
                    {
                        // Redact before truncating: otherwise a secret crossing
                        // the preview boundary would leave a visible fragment.
                        let text = vault.redact(text);
                        let preview = rook_llm::truncate(&text, 4096);
                        work.insert(key.into(), json!(preview));
                        work.insert(format!("{key}_bytes"), json!(text.len()));
                        work.insert(format!("{key}_truncated"), json!(preview.len() < text.len()));
                    }
                }
            }
            return Ok(result);
        }
        let identity = receipt.run.as_ref().ok_or_else(|| {
            bad("remote task is still pending; inspect status before messaging or reading its result")
        })?;
        let saved = managed::read(self, &identity.id)?;
        if saved.run.identity() != *identity {
            return Err(bad(
                "remote task generation changed; original receipt cannot control the resumed generation",
            ));
        }
        match operation {
            Operation::Message { text, .. } => {
                let text = vault.redact(text);
                let quote = crate::sources::data(
                    "remote_agent_message",
                    &format!("authenticated route {route}"),
                    &json!({"sender_session":sender,"delivery":delivery,"text":text}).to_string(),
                );
                let key = ObjectId::of(format!("{route}\0{delivery}\0{id}").as_bytes()).to_hex();
                let receipt =
                    managed::steer_peer(self, identity, rook_proto::work::Steer { id: key, text: quote })?;
                Ok(json!({"id":id,"run":identity,"message":receipt}))
            }
            Operation::Cancel { .. } => {
                let control = managed::control_identified(
                    self,
                    &identity.id,
                    rook_proto::work::IdentifiedControl {
                        id: ObjectId::of(format!("{route}\0{delivery}\0{id}").as_bytes()).to_hex(),
                        generation: identity.generation.clone(),
                        action: rook_proto::work::Action::Cancel,
                    },
                )?;
                Ok(json!({"id":id,"control":control}))
            }
            Operation::Result { offset, source, .. } => {
                let reply = vault.redact(&saved.run.reply);
                let digest = ObjectId::of(reply.as_bytes()).to_hex();
                if *offset > reply.len()
                    || !reply.is_char_boundary(*offset)
                    || (*offset > 0 && source.as_deref() != Some(&digest))
                    || source.as_ref().is_some_and(|source| source != &digest)
                {
                    return Err(bad(
                        "remote result changed or cursor is invalid; restart at offset 0 without source",
                    ));
                }
                let mut end = offset.saturating_add(4096).min(reply.len());
                while !reply.is_char_boundary(end) {
                    end -= 1;
                }
                Ok(json!({"id":id,"run":identity,"status":saved.run.status,"source":digest,
                    "offset":offset,"total_bytes":reply.len(),"text":&reply[*offset..end],
                    "next_offset":(end<reply.len()).then_some(end)}))
            }
            _ => Err(bad("invalid remote operation")),
        }
    }
}

fn redact_response(value: &mut Value, vault: &crate::Vault) {
    match value {
        Value::String(text) => *text = vault.redact(text),
        Value::Array(values) => {
            for value in values {
                redact_response(value, vault);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                redact_response(value, vault);
            }
        }
        _ => {}
    }
}
