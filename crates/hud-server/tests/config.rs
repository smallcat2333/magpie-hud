use magpie_hud_server::ServerConfig;

fn valid() -> ServerConfig {
    let root = std::env::temp_dir();
    ServerConfig {
        listen: "127.0.0.1:3435".parse().unwrap(),
        usage_jsonl_path: root.join("usage.jsonl"),
        magpie_host: "127.0.0.1:3425".into(),
        data_dir: root.join("hud"),
        public_url: None,
        timezone: None,
        caller_key_aliases: Default::default(),
    }
}
#[test]
fn rejects_unsafe_listening_and_relative_paths() {
    let mut config = valid();
    assert!(config.validate().is_ok());
    config.listen = "0.0.0.0:3435".parse().unwrap();
    assert!(
        config.validate().is_err(),
        "exposed listeners require an HTTPS public URL"
    );
    config.public_url = Some("http://hud.example.com".into());
    assert!(config.validate().is_err(), "public URL must use HTTPS");
    config.public_url = Some("https://hud.example.com".into());
    assert!(config.validate().is_ok());
    config.public_url = Some("https://hud.example.com/path".into());
    assert!(config.validate().is_err());
    config.public_url = None;
    config.usage_jsonl_path = "usage.jsonl".into();
    assert!(config.validate().is_err());
    config = valid();
    config.magpie_host = "10.0.0.2:3425".into();
    assert!(config.validate().is_err(), "Magpie must stay on loopback");
}

#[test]
fn shipped_example_configuration_parses() {
    let text = include_str!("../../../deploy/config.example.json");
    let config: ServerConfig =
        serde_json::from_str(text).expect("example config must match ServerConfig");
    assert_eq!(config.listen.port(), 3435);
    assert!(config.public_url.is_some());
}
