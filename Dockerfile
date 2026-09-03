# syntax=docker/dockerfile:1.9
#
# The release workflow supplies these pre-built binaries:
#   tmp/docker-context/amd64/twin-openai
#   tmp/docker-context/arm64/twin-openai

FROM ubuntu:22.04@sha256:2edbbc5dc405e9612ba3584ce95480277e3eb374407b5505fe26f17df77c7dbc

ARG TARGETARCH

COPY --chmod=0755 tmp/docker-context/${TARGETARCH}/twin-openai /usr/local/bin/twin-openai

ENV TWIN_OPENAI_BIND_ADDR=0.0.0.0:3000

EXPOSE 3000

USER 65532:65532

ENTRYPOINT ["/usr/local/bin/twin-openai"]
