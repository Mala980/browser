# Astra browser - build
#
# make            -> build/astra
# make test       -> build unit + integration tests and run them
# make install    -> install to /usr/local/bin (or PREFIX=...)
# make termux     -> cross compile / native build script for Termux (see scripts/build-termux.sh)

CC      ?= cc
CFLAGS  ?= -std=gnu11 -O2 -g -Wall -Wextra -Wno-unused-parameter -Wno-unused-function -Wno-format-truncation \
           -fno-omit-frame-pointer -D_GNU_SOURCE
LDFLAGS ?=
PREFIX  ?= /usr/local

BUILD   := build
SRC     := $(wildcard src/*.c)
MAINOBJ := $(BUILD)/src/main.o
LIBOBJ  := $(filter-out $(MAINOBJ),$(patsubst src/%.c,$(BUILD)/src/%.o,$(SRC)))
TARGET  := $(BUILD)/astra

.PHONY: all clean test integration measure test-all install termux package

# NOTE: `make build` is a no-op - `build` is the output directory, so make
# considers that target up to date.  Use `make` (or `make all`) to compile.
all: $(TARGET)

$(BUILD)/src/%.o: src/%.c | $(BUILD)
	@mkdir -p $(dir $@)
	$(CC) $(CFLAGS) -Isrc -c $< -o $@

$(TARGET): $(LIBOBJ) $(MAINOBJ)
	$(CC) $(CFLAGS) $^ -o $@ $(LDFLAGS) -lm -lpthread

$(BUILD):
	@mkdir -p $(BUILD)/src

test: $(TARGET) $(BUILD)/astra-test
	./$(BUILD)/astra-test
	@echo "unit tests ok"

integration: $(TARGET)
	python3 tests/e2e/test_local.py

measure: $(TARGET)
	python3 tests/e2e/measure_savings.py

test-all: test integration measure
	@echo "all local tests passed"

$(BUILD)/astra-test: $(LIBOBJ) tests/unit/test_all.c | $(BUILD)
	$(CC) $(CFLAGS) -Isrc -Isrc/.. $^ -o $@ $(LDFLAGS) -lm -lpthread

install: $(TARGET)
	install -d $(DESTDIR)$(PREFIX)/bin
	install -m 0755 $(TARGET) $(DESTDIR)$(PREFIX)/bin/astra

termux:
	@bash scripts/build-termux.sh --package

package: $(TARGET)
	@bash scripts/package-deb.sh 0.1.0 $(shell uname -m) build/astra

clean:
	rm -rf $(BUILD)
