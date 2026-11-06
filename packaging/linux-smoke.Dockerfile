# Smoke the released host-glibc payload, never a Nix closure or installed host package.
FROM ubuntu:24.04
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install --yes --no-install-recommends \
      nodejs libc-bin libgcc-s1 util-linux \
 && rm -rf /var/lib/apt/lists/*
