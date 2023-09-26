# docker build --build-arg GITHUB_AUTH_TOKEN=... -t vladsimplestakingcom/openmina-archive:0.1 .

FROM rust:1.71-buster as builder

RUN rustup update nightly-2023-06-01 && rustup default nightly-2023-06-01
RUN apt update && apt install -y libclang-dev clang
RUN mkdir -p -m 0700 ~/.ssh && ssh-keyscan github.com >> ~/.ssh/known_hosts

ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

ARG GITHUB_AUTH_TOKEN
RUN git config --global url."https://${GITHUB_AUTH_TOKEN}:@github.com/".insteadOf "https://github.com/" && \
    cargo install --git https://github.com/openmina/openmina-archive openmina-archive --locked && \
    rm ~/.gitconfig

FROM debian:buster

RUN apt-get update && apt-get install -y libssl-dev

COPY --from=builder /usr/local/cargo/bin/openmina-archive \
    /usr/local/bin/openmina-archive
