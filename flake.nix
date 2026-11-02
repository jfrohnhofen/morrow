{
  description = "morrow — a Rust sunrise alarm firmware for the Raspberry Pi Pico W (RP2040)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Source of the CYW43439 WiFi firmware blobs. Pinned by flake.lock so the
    # binaries stay out of git while the build stays reproducible.
    embassy = {
      url = "github:embassy-rs/embassy";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, embassy }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        target = "thumbv6m-none-eabi";

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          targets = [ target ];
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" "llvm-tools" ];
        };

        # The blobs cyw43 needs. btfw is Bluetooth-only, so it is omitted.
        cyw43Firmware = pkgs.runCommand "cyw43-firmware" { } ''
          mkdir -p $out
          cp ${embassy}/cyw43-firmware/43439A0.bin $out/
          cp ${embassy}/cyw43-firmware/43439A0_clm.bin $out/
          cp ${embassy}/cyw43-firmware/nvram_rp2040.bin $out/
        '';

        # build.rs reads the current POSIX timezone rules out of this.
        tzdata = pkgs.tzdata;

        hostTarget = pkgs.stdenv.hostPlatform.rust.rustcTarget;

        nativeTools = with pkgs; [
          rustToolchain
          curl
          python3
          probe-rs-tools
          picotool
          elf2uf2-rs
          flip-link
          pkg-config
          udev
          libusb1
        ];

        # Serves the web UI on the host against mock state, so the pages can be
        # developed in a browser without flashing anything.
        devserverScript = pkgs.writeShellApplication {
          name = "morrow-devserver";
          runtimeInputs = nativeTools;
          text = ''
            set -euo pipefail
            export TZDIR="${tzdata}/share/zoneinfo"
            exec cargo run --quiet -p morrow-devserver --target ${hostTarget} -- "$@"
          '';
        };

        # Everything that can be checked without hardware.
        testScript = pkgs.writeShellApplication {
          name = "morrow-test";
          runtimeInputs = nativeTools ++ [ pkgs.python3 pkgs.coreutils ];
          text = ''
            set -euo pipefail
            export TZDIR="${tzdata}/share/zoneinfo"
            export CYW43_FIRMWARE_DIR="${cyw43Firmware}"

            echo "==> firmware builds for ${target}"
            cargo build --release --quiet

            echo "==> bootloader builds for ${target}"
            cargo build --release --quiet -p morrow-bootloader

            echo "==> clippy is clean"
            cargo clippy --release --quiet -- -D warnings
            cargo clippy --release --quiet -p morrow-bootloader -- -D warnings

            echo "==> flash layout is consistent"
            OBJCOPY="$(find "$(rustc --print sysroot)" \( -type f -o -type l \) -name llvm-objcopy | head -1)"
            "$OBJCOPY" -O binary "target/${target}/release/morrow" target/morrow-app.bin
            "$OBJCOPY" -O binary "target/${target}/release/morrow-bootloader" target/morrow-boot.bin
            python3 tools/check_layout.py

            echo "==> uf2 packer round-trips"
            python3 tools/test_uf2.py

            echo "==> web ui unit tests"
            cargo test --quiet -p morrow-ui --features std --target ${hostTarget}

            echo "==> timezone rules match glibc"
            python3 tools/test_tz.py

            echo "==> settings survive a flash round-trip"
            bash tools/test_settings_codec.sh

            echo ""
            echo "all checks passed"
          '';
        };

        flashScript = pkgs.writeShellApplication {
          name = "morrow-flash";
          runtimeInputs = nativeTools;
          text = ''
            set -euo pipefail
            export TZDIR="${tzdata}/share/zoneinfo"
            export CYW43_FIRMWARE_DIR="${cyw43Firmware}"

            TARGET_DIR="target/${target}/release"
            OUT="target"
            # The rust toolchain ships llvm-objcopy as a symlink, so match links too.
            OBJCOPY="$(find "$(rustc --print sysroot)" \( -type f -o -type l \) -name llvm-objcopy | head -1)"
            if [ -z "$OBJCOPY" ]; then
              echo "llvm-objcopy not found in the rust sysroot" >&2
              exit 1
            fi

            build_app() {
              cargo build --release --quiet
              "$OBJCOPY" -O binary "$TARGET_DIR/morrow" "$OUT/morrow-app.bin"
            }

            build_boot() {
              cargo build --release --quiet -p morrow-bootloader
              "$OBJCOPY" -O binary "$TARGET_DIR/morrow-bootloader" "$OUT/morrow-boot.bin"
            }

            case "''${1:-help}" in
              provision)
                # Everything a blank board needs, in one drag-and-drop: the
                # bootloader, the WiFi blobs, and the firmware itself. Only
                # needed once; after this, updates go over the air.
                build_boot
                build_app
                BLOBS=""
                while read -r addr len path; do
                  [ -n "$addr" ] || continue
                  BLOBS="$BLOBS $addr=$path"
                  echo "  blob $addr  $len bytes"
                done < "$OUT/blobs.manifest"
                # shellcheck disable=SC2086
                python3 tools/mkuf2.py "$OUT/morrow-provision.uf2" \
                  "0x10000000=$OUT/morrow-boot.bin" \
                  "0x10007000=$OUT/morrow-app.bin" \
                  $BLOBS
                echo ""
                echo "==> $OUT/morrow-provision.uf2"
                echo "    Hold BOOTSEL while plugging in, then copy it to RPI-RP2."
                ;;

              ota)
                HOST="''${2:-morrow.local}"
                build_app
                SIZE=$(stat -c%s "$OUT/morrow-app.bin")
                CRC=$(python3 -c "import zlib,sys;print('%08x' % (zlib.crc32(open(sys.argv[1],'rb').read()) & 0xffffffff))" "$OUT/morrow-app.bin")
                echo "==> uploading $SIZE bytes to http://$HOST/ota (crc $CRC)"
                curl -fsS --max-time 180 -X POST \
                  -H "X-Image-Crc32: $CRC" \
                  -H "Content-Type: application/octet-stream" \
                  --data-binary "@$OUT/morrow-app.bin" \
                  "http://$HOST/ota"
                echo "    The device reboots into the new image and confirms it"
                echo "    after 30 s. If it does not come back, it reverts by itself."
                ;;

              app)
                # The firmware alone, for a board that already has a bootloader.
                build_app
                python3 tools/mkuf2.py "$OUT/morrow-app.uf2" "0x10007000=$OUT/morrow-app.bin"
                echo "==> $OUT/morrow-app.uf2"
                ;;

              probe)
                echo "==> cargo run (probe-rs, needs a debug probe on SWD)"
                exec cargo run --release
                ;;

              picotool)
                build_boot
                build_app
                echo "==> loading over USB in BOOTSEL mode"
                picotool load -o 0x10000000 "$OUT/morrow-boot.bin"
                picotool load -o 0x10007000 "$OUT/morrow-app.bin"
                while read -r addr len path; do
                  [ -n "$addr" ] || continue
                  echo "  blob $addr  $len bytes"
                  picotool load -o "$addr" "$path"
                done < "$OUT/blobs.manifest"
                picotool reboot
                ;;

              *)
                cat <<'USAGE'
            morrow-flash <command>

              provision      one UF2 with bootloader + WiFi blobs + firmware,
                             for a blank board (drag onto RPI-RP2)
              ota [host]     build and upload over the air (default morrow.local)
              app            firmware-only UF2, for a board that has a bootloader
              probe          flash and stream defmt logs over SWD
              picotool       load every region over USB in BOOTSEL mode
            USAGE
                ;;
            esac
          '';
        };

      in
      {
        packages = {
          default = flashScript;
          flash = flashScript;
          devserver = devserverScript;
          test = testScript;
          inherit cyw43Firmware;
        };

        checks.test = testScript;

        devShells.default = pkgs.mkShell {
          name = "morrow";
          # The two helper scripts go on PATH rather than being shell functions,
          # so they survive direnv into whatever shell you actually use.
          packages = nativeTools ++ [ devserverScript testScript flashScript pkgs.python3 ];

          CYW43_FIRMWARE_DIR = "${cyw43Firmware}";
          TZDIR = "${tzdata}/share/zoneinfo";
          CARGO_BUILD_TARGET = target;

          shellHook = ''
            echo "────────────────────────────────────────────────────────"
            echo " morrow — sunrise alarm firmware (Pico W / RP2040)"
            echo "────────────────────────────────────────────────────────"
            echo " target     ${target}"
            echo " cyw43 fw   $CYW43_FIRMWARE_DIR"
            echo " tzdata     $TZDIR"
            echo ""
            echo " cargo build --release        compile the firmware"
            echo " morrow-test                  run every off-device check"
            echo " morrow-devserver             web ui on http://127.0.0.1:8080"
            echo " morrow-flash provision       one UF2 for a blank board"
            echo " morrow-flash ota [host]      build and update over the air"
            echo " morrow-flash app             firmware-only UF2"
            echo " morrow-flash probe           flash + RTT logs via probe-rs"
            echo ""
            echo " Same things from outside the shell: nix run .#test, .#devserver, .#flash"
            echo "────────────────────────────────────────────────────────"
          '';
        };
      }
    );
}
