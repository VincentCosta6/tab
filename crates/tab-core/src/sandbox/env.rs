//! What's left of tab's environment for a process it starts.
//!
//! tab often runs with tokens in its environment (a `GH_TOKEN` from the
//! shell that started it, cloud credentials). Agents and commands don't
//! need them: tab's MCP tools make service calls with tab's own
//! credentials. So anything that looks like a secret is removed, except the
//! few a process needs to do its job.

use super::Trust;

/// Words (between underscores) that mark a variable as secret:
/// `GH_TOKEN`, `STRIPE_SECRET_KEY`, `SENTRY_DSN`, but not `GIT_AUTHOR_NAME`.
const SECRET_WORDS: &[&str] = &[
    "TOKEN",
    "TOKENS",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "PASS",
    "PWD",
    "KEY",
    "APIKEY",
    "CREDENTIAL",
    "CREDENTIALS",
    "CREDS",
    "AUTH",
    "COOKIE",
    "DSN",
    "WEBHOOK",
    "PAT",
];

/// Whole families of credentials.
const SECRET_PREFIXES: &[&str] = &[
    "AWS_",
    "AZURE_",
    "ARM_",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GCLOUD_",
    "CLOUDSDK_",
    "DIGITALOCEAN_",
    "HEROKU_",
    "VAULT_",
    "KUBECONFIG",
    "DOCKER_AUTH",
    "NPM_CONFIG__AUTH",
    "PGPASSWORD",
    "MYSQL_PWD",
];

/// Never secret, even though the name says so.
const ALWAYS_KEEP: &[&str] = &["SSH_AUTH_SOCK", "XAUTHORITY", "GIT_ASKPASS", "SSH_ASKPASS"];

/// What the agent CLI itself signs in with.
const MODEL_KEYS: &[&str] = &[
    "ANTHROPIC_",
    "CLAUDE_",
    "OPENAI_",
    "OPENROUTER_",
    "GEMINI_",
    "GOOGLE_API_KEY",
    "GOOGLE_GENERATIVE_AI_API_KEY",
    "GROQ_",
    "MISTRAL_",
    "DEEPSEEK_",
    "XAI_",
    "OPENCODE_",
];

/// The person's own git host token, so their chats can push and open PRs.
const PERSON_KEYS: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GLAB_TOKEN",
    "GITLAB_TOKEN",
];

/// GitHub tokens. When tab brokers `gh` and git for a chat, these stay out
/// of the agent's environment even in the person's own chats.
pub const GITHUB_KEYS: &[&str] = &["GH_TOKEN", "GITHUB_TOKEN", "GH_ENTERPRISE_TOKEN"];

pub fn is_secret(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    if ALWAYS_KEEP.contains(&n.as_str()) {
        return false;
    }
    if n == "PWD" || n == "OLDPWD" {
        return false;
    }
    SECRET_PREFIXES.iter().any(|p| n.starts_with(p))
        || n.split('_').any(|w| SECRET_WORDS.contains(&w))
}

/// Whether `name` survives for a process trusted this much.
pub fn keeps(name: &str, trust: Trust) -> bool {
    // The SSH agent signs with the person's keys: only their own chats.
    if name.eq_ignore_ascii_case("SSH_AUTH_SOCK") {
        return trust == Trust::Person;
    }
    if !is_secret(name) {
        return true;
    }
    let n = name.to_ascii_uppercase();
    let model = MODEL_KEYS.iter().any(|p| n.starts_with(p));
    // Claude Code on Bedrock or Vertex signs in with the cloud's own keys.
    let cloud_agent = (n.starts_with("AWS_")
        && std::env::var_os("CLAUDE_CODE_USE_BEDROCK").is_some())
        || ((n.starts_with("GOOGLE_APPLICATION_CREDENTIALS") || n.starts_with("CLOUDSDK_"))
            && std::env::var_os("CLAUDE_CODE_USE_VERTEX").is_some());
    match trust {
        Trust::Person => model || cloud_agent || PERSON_KEYS.contains(&n.as_str()),
        Trust::Agent => model || cloud_agent,
        Trust::Untrusted => false,
    }
}

/// Removes what `trust` shouldn't see from the inherited environment.
/// Variables the caller sets explicitly afterwards are untouched.
pub fn scrub(cmd: &mut tokio::process::Command, trust: Trust) {
    for (k, _) in std::env::vars_os() {
        let Some(name) = k.to_str() else { continue };
        if !keeps(name, trust) {
            cmd.env_remove(&k);
        }
    }
}
