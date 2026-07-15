FROM rust:1-bookworm AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    ca-certificates \
    dbus \
    dbus-user-session \
    flatpak \
    gnome-keyring \
    imagemagick \
    openbox \
    tesseract-ocr \
    xdg-desktop-portal \
    xdg-desktop-portal-gtk \
    xdotool \
    xvfb \
  && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/flatpak-smoke /usr/local/bin/flatpak-smoke
ENTRYPOINT ["flatpak-smoke"]
