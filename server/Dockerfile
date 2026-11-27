# DeckCraft live sessions in a container: the server and the browser editor.
#
#   docker build -f server/Dockerfile -t deckcraft-server .
#   docker run -p 8080:8080 -v deckcraft-data:/data deckcraft-server
#
# Build from the repository root. See docs/collaboration.md for production settings.

FROM rust:1.95-bookworm AS build
WORKDIR /src
RUN rustup target add wasm32-unknown-unknown \
 && cargo install trunk --locked --version 0.22.1
# The bundled fonts come from storytold/craft-fonts (pinned; see ATTRIBUTION.md).
ARG CRAFT_FONTS_REV=c1db6d1a8ed772191241c3d185e2104be84d8691
RUN git clone https://github.com/storytold/craft-fonts /craft-fonts && git -C /craft-fonts checkout "$CRAFT_FONTS_REV"
ENV CRAFT_FONTS_DIR=/craft-fonts
COPY . .
RUN cargo build --release --locked -p deckcraft-server
RUN cd apps/deckcraft-web && trunk build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/* \
 && useradd --system --home /data --uid 10001 deckcraft && mkdir -p /data && chown deckcraft /data
COPY --from=build /src/target/release/deckcraft-server /usr/local/bin/deckcraft-server
COPY --from=build /src/dist/web /srv/deckcraft-web
USER deckcraft
ENV DECKCRAFT_DATA=/data \
    DECKCRAFT_WEB_DIR=/srv/deckcraft-web \
    DECKCRAFT_BIND=0.0.0.0:8080 \
    # In a container every request comes through Docker's network: localhost isn't the host's
    # own browser, so the dashboard always needs DECKCRAFT_ADMIN_PASSWORD.
    DECKCRAFT_TRUST_LOOPBACK=0
VOLUME /data
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s CMD curl -fsS http://127.0.0.1:8080/healthz || exit 1
CMD ["deckcraft-server"]
