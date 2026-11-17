# Build from the repository root:
#   docker build -f crates/easyssh-server/Dockerfile -t easyssh-server .
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p easyssh-server

FROM debian:bookworm-slim
RUN useradd --system --create-home --home-dir /data --shell /usr/sbin/nologin easyssh
COPY --from=build /src/target/release/easyssh-server /usr/local/bin/easyssh-server
ENV EASYSSH_LISTEN=0.0.0.0:8787 \
    EASYSSH_DB=/data/easyssh-server.db \
    EASYSSH_LOG_FORMAT=json
VOLUME /data
EXPOSE 8787
USER easyssh
ENTRYPOINT ["easyssh-server"]
