# EMBED Unit E — c-host example Makefile (for humans; the CI truth is the capi test
# `c_host_example_compiles_links_and_runs`, which builds this SAME main.c).
#
# Prereq: build the ascript-capi cdylib first:
#   (cd ../../../capi && cargo build)            # debug
#   (cd ../../../capi && cargo build --release)  # release
#
# Then:  make          # build ./c-host
#        make run      # build + run (sets the loader path to the cdylib)
#        make clean
#
# Override CAPI_PROFILE=release to link the release cdylib.

CAPI_DIR     := ../../../capi
INCLUDE_DIR  := $(CAPI_DIR)/include
CAPI_PROFILE ?= debug
LIB_DIR      := $(CAPI_DIR)/target/$(CAPI_PROFILE)

CFLAGS  += -std=c11 -Wall -Wextra -I$(INCLUDE_DIR)
LDFLAGS += -L$(LIB_DIR) -lascript_capi -Wl,-rpath,$(abspath $(LIB_DIR))

BIN := c-host

$(BIN): main.c $(INCLUDE_DIR)/ascript.h
	$(CC) $(CFLAGS) main.c -o $(BIN) $(LDFLAGS)

.PHONY: run clean
run: $(BIN)
	DYLD_LIBRARY_PATH=$(abspath $(LIB_DIR)) LD_LIBRARY_PATH=$(abspath $(LIB_DIR)) ./$(BIN)

clean:
	rm -f $(BIN)
