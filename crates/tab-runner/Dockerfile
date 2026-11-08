# tab, headless: a hub's cloud runner. Built from the repo root:
#   docker build -f crates/tab-runner/Dockerfile -t tab-runner .
# tab-infra's hub Worker starts one per hub (paid plans) as a Cloudflare
# Container, with TAB_CLOUD_URL, TAB_HUB_ID and TAB_RUNNER_TOKEN set.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p tab-runner

FROM node:22-bookworm-slim
# git for cloning the repos jobs work on, gh for pull requests, and the
# harnesses a person can pick (claude_code, opencode, codex, pi). Each runs
# with the person's own key from the hub's secrets.
RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates curl gh openssh-client ripgrep \
 && rm -rf /var/lib/apt/lists/* \
 && npm install -g @anthropic-ai/claude-code@latest opencode-ai@latest @openai/codex@latest @earendil-works/pi-coding-agent@latest \
 && npm cache clean --force \
 && useradd -m -d /home/tab tab \
 && mkdir -p /data && chown tab /data
COPY --from=build /src/target/release/tab-runner /usr/local/bin/tab-runner
USER tab
ENV HOME=/home/tab TAB_DATA=/data
VOLUME /data
CMD ["tab-runner"]
