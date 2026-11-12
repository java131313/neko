#!/bin/sh
# Regenerates dataplane gRPC code for the Go control plane.
# Requires: protoc, protoc-gen-go, protoc-gen-go-grpc
# (go install google.golang.org/protobuf/cmd/protoc-gen-go@latest
#  go install google.golang.org/grpc/cmd/protoc-gen-go-grpc@latest)
set -eu

cd "$(dirname "$0")/../.."

protoc \
  --proto_path=dataplane/proto \
  --go_out=server/internal/webrtcp/pb --go_opt=paths=source_relative \
  --go-grpc_out=server/internal/webrtcp/pb --go-grpc_opt=paths=source_relative \
  neko.proto
