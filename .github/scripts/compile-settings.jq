if $matrix.target != "aarch64-unknown-linux-gnu" then {
  # Use build-std to build a std library optimized for size and abort immediately on abort,
  # so that format string for `unwrap`/`expect`/`unreachable`/`panic` can be optimized out.
  #
  # Disable it on aarch64-unknown-linux-gnu as it caused the build to fail.
  release_build_std_args: "-Z build-std=std,panic_abort -Z build-std-features=panic_immediate_abort"
} else {
  release_build_std_args: ""
} end
|
if $for_release then {
  output: "release",
  profile: "release",
  args: ($matrix.release_build_args // .release_build_std_args),
  features: ($matrix.release_features // ["static", "rustls", "trust-dns", "fancy-no-backtrace", "log_release_max_level_debug"]),
} else {
  output: "debug",
  profile: "dev",
  args: ($matrix.debug_build_args // ""),
  features: ($matrix.debug_features // ["rustls", "fancy-with-backtrace"]),
} end
|
.rustflags = (
  if $for_release and $matrix.target == "aarch64-unknown-linux-musl" or $matrix.target == "armv7-unknown-linux-musleabihf"
  then "-C link-arg=-lgcc -Clink-arg=-static-libgcc"
  else "" end
)
|
.features = (
  if ($matrix."use-cross" // false)
  then .features // ["zstd-thin"]
  else .features end
)
|
.features = (
  if (.features | length > 0)
  then "--no-default-features --features \(.features | join(","))"
  else "" end
)
|
{
  CBIN: (if ($matrix.target | test("windows")) then "cargo-binstall.exe" else "cargo-binstall" end),
  CTOOL: (if ($matrix."use-cross" // false) then "cross" else "cargo" end),
  COUTPUT: .output,
  CARGS: "--target \($matrix.target) --profile \(.profile) \(.features) \(.args)",
  RUSTFLAGS: .rustflags,
}
|
to_entries[] | "\(.key)=\(.value)"
