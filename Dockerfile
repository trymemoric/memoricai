# memoricai — single self-hostable binary. Needs Postgres with pgvector and
# an OpenAI-compatible model endpoint at runtime (see README: Configuration).
FROM rust:1.88-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p memoricai

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/memoricai /usr/local/bin/memoricai
ENV MEMORICAI_BIND=0.0.0.0:6767
EXPOSE 6767
ENTRYPOINT ["memoricai"]
CMD ["serve"]
