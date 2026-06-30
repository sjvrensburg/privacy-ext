use gliner2_inference::*;
use std::env;

fn build_engine() -> anyhow::Result<Gliner2Engine> {
    let models_dir = env::var("PII_MODELS_DIR").expect("set PII_MODELS_DIR");
    Gliner2Engine::new(Gliner2Config { models_dir, max_width: 8, model_type: ModelType::HuggingFace })
}

fn main() -> anyhow::Result<()> {
    ort::init().with_name("probe").commit()?;
    let engine = build_engine()?;

    let samples = vec![
        "I live at 5 Elm Street.",
        "Send it to 5 Elm Street, Springfield.",
        "My address is 5 Elm Street.",
    ];

    // candidate label-set variants for "address"
    let variants: Vec<(&str, Vec<&str>)> = vec![
        ("baseline", vec!["name", "address", "email", "phone_num", "id_num", "url", "username"]),
        ("street_address", vec!["name", "street address", "email", "phone_num", "id_num", "url", "username"]),
        ("location+address", vec!["name", "address", "location", "email", "phone_num", "id_num", "url", "username"]),
        ("address_alt", vec!["name", "physical address", "email", "phone_num", "id_num", "url", "username"]),
    ];

    for thr in [0.55_f32, 0.40, 0.30, 0.20] {
        for (vname, labels) in &variants {
            let tasks = vec![SchemaTask::Entities(labels.iter().map(|s| s.to_string()).collect())];
            let params = InferenceParams { threshold: thr, flat_ner: true };
            for text in &samples {
                let (ents, _, _) = engine.extract(text, &tasks, Some(params.clone()))?;
                let addr: Vec<String> = ents.iter()
                    .map(|e| format!("{}='{}'@{:.3}", e.label, e.text, e.score))
                    .collect();
                println!("thr={:.2} [{}] \"{}\" => {:?}", thr, vname, text, addr);
            }
        }
    }
    Ok(())
}
