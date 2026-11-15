# Update these digests deliberately; see docs/security.md.
FROM rust:1-trixie@sha256:09027f873d45b8ecd1dc86f1fa13b7a62b875c961f017a0e331267b350430137 AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY desktop ./desktop
RUN cargo build --release --locked

FROM debian:trixie-slim@sha256:a72b4433de1cf7d6bae0c4cee74d0547f58601a643b3375f070c51600d63b6b2 AS runtime
RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    ca-certificates \
    dbus \
    dbus-user-session \
    flatpak \
    gnome-keyring \
    imagemagick \
    tesseract-ocr \
    weston \
    xwayland \
    x11-utils \
    xdg-desktop-portal \
    xdg-desktop-portal-gtk \
  && printf 'auth required pam_permit.so\naccount required pam_permit.so\n' > /etc/pam.d/weston-remote-access \
  && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/flatpak-smoke /usr/local/bin/flatpak-smoke
ENTRYPOINT ["flatpak-smoke"]

FROM builder AS installer-test-builder
RUN cargo test --locked --lib --no-run \
  && find target/debug/deps -maxdepth 1 -type f -executable -name 'flatpak_smoke-*' \
       -exec cp {} /installer-tests \;

FROM runtime AS e2e-tests
COPY --from=installer-test-builder /installer-tests /usr/local/bin/installer-tests
RUN apt-get update \
  && apt-get install -y --no-install-recommends python3 \
  && rm -rf /var/lib/apt/lists/*
# Install runtimes as a real container system installation, rather than treating
# a host user installation as one through a read-only bind mount.
RUN flatpak remote-add --system flathub https://flathub.org/repo/flathub.flatpakrepo \
  && flatpak install --system --noninteractive flathub org.gnome.Platform//49

FROM runtime AS final
