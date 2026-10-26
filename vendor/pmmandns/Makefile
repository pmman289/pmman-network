GO ?= go
TARGET ?= pmmandns

.PHONY: test build linux-arm linux-arm64

test:
	$(GO) test ./...

build:
	CGO_ENABLED=0 $(GO) build -trimpath -ldflags='-s -w' -o $(TARGET) ./cmd/pmmandns

linux-arm:
	CGO_ENABLED=0 GOOS=linux GOARCH=arm GOARM=7 $(GO) build -trimpath -ldflags='-s -w' -o $(TARGET)-armv7 ./cmd/pmmandns

linux-arm64:
	CGO_ENABLED=0 GOOS=linux GOARCH=arm64 $(GO) build -trimpath -ldflags='-s -w' -o $(TARGET)-arm64 ./cmd/pmmandns
