#!/bin/sh
# Builds a release of Ichibou for this machine and packs it as
# dist/ichibou-<version>-<os>-<arch>.tar.gz, with its SHA-256 beside it.
# The binary carries the UI inside it; nothing else needs to be shipped.
set -eu
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)
name="ichibou-$version-$os-$arch"

npm --prefix ui ci
npm --prefix ui run build
cargo build --release -p ichibou-cli

rm -rf "dist/$name"
mkdir -p "dist/$name"
cp target/release/ichibou LICENSE README.md README.ja.md README.zh-CN.md "dist/$name/"
tar -C dist -czf "dist/$name.tar.gz" "$name"
rm -rf "dist/$name"
shasum -a 256 "dist/$name.tar.gz" | tee "dist/$name.tar.gz.sha256"
