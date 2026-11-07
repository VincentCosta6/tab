use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use serde_json::json;

use super::*;
use crate::decisions::keys::opencode_key;
use tab_domain::decision::ranking;

/// Answers each request with the next canned JSON body, and keeps what it saw.
pub(crate) fn serve(bodies: Vec<String>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    std::thread::spawn(move || {
        for (stream, body) in listener.incoming().zip(bodies) {
            let mut stream = stream.unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 8192];
            // Read headers, then the body by content-length.
            loop {
                let n = stream.read(&mut buf).unwrap();
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if req.len() >= end + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            s.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&req).to_string());
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(resp.as_bytes()).unwrap();
        }
    });
    (addr, seen)
}

fn cfg(provider: &str, base: &str) -> DecisionConfig {
    DecisionConfig {
        provider: provider.into(),
        model: None,
        api_keys: [(provider.to_string(), "test-key".to_string())].into(),
        key_from: Default::default(),
        base_url: Some(base.into()),
    }
}

fn body_of(req: &str) -> Value {
    serde_json::from_str(&req[req.find("\r\n\r\n").unwrap() + 4..]).unwrap()
}

#[test]
fn every_catalog_entry_has_an_adapter_and_models() {
    let ids: Vec<&str> = providers().iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["openrouter", "typesafe", "openai", "agent"]);
    for p in providers() {
        assert!(!p.models.is_empty(), "{} has no models", p.id);
        for m in &p.models {
            // The ranker's questions must fit every model tab offers.
            assert!(
                tab_domain::decision::validate(&ranking::questions(), &m.capabilities).is_empty(),
                "{} can't take the ranking questions",
                m.id
            );
        }
    }
    let luna = &provider("openai").unwrap().models[0];
    assert_eq!(
        (luna.id.as_str(), luna.family.as_str()),
        ("gpt-6-luna", "Luna")
    );
}

#[test]
fn system_one_round_trip() {
    let qs = ranking::questions();
    let req = system_one::request("typesafe/jev-1.14", &json!({"item": "x"}), &qs);
    assert_eq!(req["questions"]["urgency"]["type"], "score");
    assert_eq!(
        req["questions"]["urgency"]["criteria"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert_eq!(req["questions"]["addressed"]["type"], "noul");

    let resp = json!({
        "model": "jev-1.14.0",
        "answers": {
            "urgency": {"type": "score", "score": 2.4, "confidence": 0.8,
                        "probabilities": {"0": 0.0, "1": 0.1, "2": 0.4, "3": 0.5}},
            "addressed": {"type": "noul", "noul": 0.9}
        },
        "usage": {"input_tokens": 1200, "output_tokens": 3, "cost": 0.00005}
    });
    let (answers, usage) = system_one::response(&resp, &qs).unwrap();
    assert_eq!(
        answers[0].1,
        Answer::Score {
            score: 2.4,
            confidence: Some(0.8),
            probabilities: vec![
                ("informational".into(), 0.0),
                ("this_week".into(), 0.1),
                ("today".into(), 0.4),
                ("now".into(), 0.5)
            ],
        }
    );
    assert_eq!(answers[1].1, Answer::YesNo { probability: 0.9 });
    // Missing answers are refusals, not errors.
    assert_eq!(answers[2].1, Answer::Refused);
    assert_eq!(usage.cost_usd, Some(0.00005));

    let choice = vec![Question::choice(
        "dept",
        "Who handles it?",
        vec![Opt::new("billing", "Money"), Opt::new("shipping", "Boxes")],
    )];
    let req = system_one::request("jev-latest", &json!("text"), &choice);
    assert_eq!(req["questions"]["dept"]["criteria"]["billing"], "Money");
    let resp = json!({"answers": {"dept": {"type": "choice", "choice": "billing", "confidence": 1.0,
                      "probabilities": {"billing": 1.0, "shipping": 0.0}}}});
    let (a, _) = system_one::response(&resp, &choice).unwrap();
    assert!(matches!(&a[0].1, Answer::Choice { choice, .. } if choice == "billing"));
    // An option we didn't offer is refused.
    let resp = json!({"answers": {"dept": {"type": "choice", "choice": "returns"}}});
    assert_eq!(
        system_one::response(&resp, &choice).unwrap().0[0].1,
        Answer::Refused
    );
}

#[test]
fn openai_round_trip() {
    let qs = ranking::questions();
    let req = openai::request("gpt-6-luna", &json!({"item": "x"}), &qs);
    assert_eq!(req["model"], "gpt-6-luna");
    assert!(req["input"].as_str().unwrap().contains("\"item\""));
    assert_eq!(req["questions"][0]["type"], "score");
    assert_eq!(req["questions"][0]["name"], "urgency");
    assert_eq!(req["questions"][0]["levels"][2]["label"], "today");
    assert_eq!(req["questions"][1]["type"], "predicate");

    let resp = json!({
        "answers": [
            {"type": "score", "name": "urgency", "score": 1.1, "confidence": 0.55,
             "probabilities": [{"value": 1, "label": "this_week", "probability": 0.7}]},
            {"type": "predicate", "name": "addressed", "probability": 0.92},
            {"type": "refusal", "name": "blocking"}
        ],
        "usage": {"input_tokens": 900, "input_tokens_details": {"cached_tokens": 0}}
    });
    let (answers, usage) = openai::response(&resp, &qs).unwrap();
    let Answer::Score {
        score,
        confidence,
        probabilities,
    } = &answers[0].1
    else {
        panic!("{:?}", answers[0])
    };
    assert_eq!((*score, *confidence), (1.1, Some(0.55)));
    assert_eq!(probabilities[1], ("this_week".into(), 0.7));
    assert_eq!(answers[1].1, Answer::YesNo { probability: 0.92 });
    assert_eq!(answers[2].1, Answer::Refused);
    assert_eq!((usage.input_tokens, usage.cost_usd), (900, None));
}

#[test]
fn reads_only_api_keys_from_opencode() {
    let auth = r#"{"openrouter": {"type": "api", "key": " sk-or-1 "},
                   "openai": {"type": "oauth", "access": "tok", "refresh": "r"}}"#;
    assert_eq!(opencode_key(auth, "openrouter").as_deref(), Some("sk-or-1"));
    // A ChatGPT sign-in is a subscription, not a key.
    assert_eq!(opencode_key(auth, "openai"), None);
    assert_eq!(opencode_key(auth, "typesafe"), None);
    assert_eq!(opencode_key("not json", "openrouter"), None);
}

#[test]
fn no_provider_or_no_key_means_no_decider() {
    let store = Arc::new(Store::in_memory().unwrap());
    let gw = Arc::new(Gateway::new(store.clone()));
    assert!(
        Decider::from_config(&DecisionConfig::default(), gw.clone(), store.clone(), None).is_none()
    );
    let mut c = cfg("typesafe", "http://127.0.0.1:1");
    c.api_keys.clear();
    // Only checkable when the environment has no key of its own.
    if std::env::var("TYPESAFE_API_KEY").is_err() {
        assert!(Decider::from_config(&c, gw.clone(), store.clone(), None).is_none());
    }
    let mut c = cfg("openrouter", "http://127.0.0.1:1");
    c.model = Some("not-a-model".into());
    assert!(Decider::from_config(&c, gw, store, None).is_none());
}

#[tokio::test]
async fn decides_through_the_gateway_and_records_spend() {
    let body = json!({
        "answers": {"urgency": {"type": "score", "score": 3.0, "confidence": 0.9, "probabilities": {}},
                    "addressed": {"type": "noul", "noul": 0.2},
                    "blocking": {"type": "noul", "noul": 0.95}},
        "usage": {"input_tokens": 1000, "output_tokens": 0}
    });
    let (base, seen) = serve(vec![body.to_string()]);
    let store = Arc::new(Store::in_memory().unwrap());
    let gw = Arc::new(Gateway::new(store.clone()));
    let d = Decider::from_config(&cfg("typesafe", &base), gw, store.clone(), None).unwrap();
    assert_eq!(d.model().id, "jev-latest");
    let answers = d
        .decide(
            &json!({"item": "prod down"}),
            &ranking::questions(),
            "ranker",
        )
        .await
        .unwrap();
    assert_eq!(answers.len(), 3);
    assert_eq!(answers[2].1, Answer::YesNo { probability: 0.95 });

    let req = seen.lock().unwrap()[0].clone();
    assert!(req.starts_with("POST /v1/systemone"));
    assert!(
        req.to_lowercase()
            .contains("authorization: bearer test-key")
    );
    assert_eq!(body_of(&req)["state"]["item"], "prod down");

    // 1000 input tokens at $0.042 per million, priced since TypeSafe sends no cost.
    let today = store.usage_summary(Utc::now()).unwrap().today_usd;
    assert!((today - 0.000042).abs() < 1e-9, "{today}");
}

#[tokio::test]
async fn refuses_questions_the_model_cant_take() {
    let store = Arc::new(Store::in_memory().unwrap());
    let gw = Arc::new(Gateway::new(store.clone()));
    let d = Decider::from_config(&cfg("openai", "http://127.0.0.1:1"), gw, store, None).unwrap();
    let err = d
        .decide(&json!({}), &[Question::choice("x", "?", vec![])], "test")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("2 to 255 options"), "{err}");
}

#[test]
fn llm_dialect_asks_for_offered_values_only() {
    let qs = ranking::questions();
    let schema = llm::schema(&qs);
    assert_eq!(
        schema["properties"]["urgency"]["properties"]["level"]["enum"],
        json!(["informational", "this_week", "today", "now"])
    );
    assert_eq!(
        schema["required"],
        json!(["urgency", "addressed", "blocking"])
    );
    let prompt = llm::prompt(&json!({"item": "prod down"}), &qs);
    assert!(prompt.contains("prod down"));
    assert!(prompt.contains("- urgency (score):"));
    assert!(prompt.contains("    - today: "));

    let reply = json!({
        "urgency": {"level": "today", "confidence": 0.7},
        "addressed": {"probability": 1.4},
        "blocking": {"probability": "high"}
    });
    let a = llm::answers(&reply, &qs);
    let Answer::Score {
        score,
        confidence,
        probabilities,
    } = &a[0].1
    else {
        panic!("{:?}", a[0])
    };
    // A whole level, and the model's own confidence.
    assert_eq!((*score, *confidence), (2.0, Some(0.7)));
    assert_eq!(probabilities[2], ("today".into(), 0.7));
    assert_eq!(a[1].1, Answer::YesNo { probability: 1.0 });
    assert_eq!(a[2].1, Answer::Refused);
    // A level we didn't offer is refused.
    let a = llm::answers(&json!({"urgency": {"level": "soon", "confidence": 1}}), &qs);
    assert_eq!(a[0].1, Answer::Refused);
}

#[test]
fn the_agent_provider_needs_agents_but_no_key() {
    let p = provider("agent").unwrap();
    assert!(p.via_agent());
    let store = Arc::new(Store::in_memory().unwrap());
    let gw = Arc::new(Gateway::new(store.clone()));
    let c = DecisionConfig {
        provider: "agent".into(),
        model: Some("opencode".into()),
        ..Default::default()
    };
    // Without a hub to ask through, it's off.
    assert!(Decider::from_config(&c, gw, store, None).is_none());
    let s = status(&c);
    assert!(s.iter().find(|s| s.provider.id == "agent").unwrap().has_key);
}
