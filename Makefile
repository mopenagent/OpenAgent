SERVICES := discord telegram slack hello

PLATFORMS := \
	linux/arm64 \
	linux/amd64 \
	darwin/arm64

BIN_DIR := bin

.PHONY: all clean $(SERVICES)

all: $(SERVICES)

# Build all platforms for a single service.
define build_service
$(1):
	@mkdir -p $(BIN_DIR)
	@echo "==> $(1)"
	cd services/$(1) && GOOS=linux  GOARCH=arm64 go build -o ../../$(BIN_DIR)/$(1)-linux-arm64  .
	cd services/$(1) && GOOS=linux  GOARCH=amd64 go build -o ../../$(BIN_DIR)/$(1)-linux-amd64  .
	cd services/$(1) && GOOS=darwin GOARCH=arm64 go build -o ../../$(BIN_DIR)/$(1)-darwin-arm64 .
endef

$(foreach svc,$(SERVICES),$(eval $(call build_service,$(svc))))

clean:
	rm -rf $(BIN_DIR)
