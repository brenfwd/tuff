# LOCAL BUILDS ONLY. The image embeds data generated from Mojang's server jar;
# never push it to a public registry.
#   docker build --build-arg ACCEPT_MOJANG_EULA=1 -t tuff .
#   add --build-arg OPT=1 for the release-opt profile (fat LTO; slower build)
#   docker run -p 25565:25565 -v tuff-data:/data tuff
# Accepting means you accept https://www.minecraft.net/eula.
FROM debian:bookworm-slim AS builder
ARG ACCEPT_MOJANG_EULA=""
ARG JOBS=""
ARG OPT=""
RUN test "$ACCEPT_MOJANG_EULA" = 1 || { echo "set --build-arg ACCEPT_MOJANG_EULA=1 to accept the Minecraft EULA (https://www.minecraft.net/eula)" >&2; exit 1; }
RUN apt-get update && apt-get install -y --no-install-recommends build-essential git curl ca-certificates unzip pkg-config && rm -rf /var/lib/apt/lists/*
COPY . /src
WORKDIR /src
ENV TUFF_ACCEPT_MOJANG_EULA=1
RUN scripts/install.sh --accept-eula --no-update --prefix /out ${JOBS:+--jobs $JOBS} $([ "$OPT" = 1 ] && echo --opt) --clean

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /out/tuff /usr/local/bin/tuff
WORKDIR /data
EXPOSE 25565
ENTRYPOINT ["tuff"]
