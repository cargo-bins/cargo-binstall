if true then {
  output: "release",
  profile: "release",
  # Use build-std to build a std library optimized for size and abort immediately on abort,
  # so that format string for `unwrap`/`expect`/`unreachable`/`panic` can be optimized out.
  args: ($matrix.release_build_args // "-Z build-std=std,panic_abort -Z build-std-features=panic_immediate_abort"),
  features: ($matrix.release_features // []),
  rustflags: ($matrix.release_rustflags // (
    if $matrix.target == "aarch64-unknown-linux-musl" or $matrix.target == "armv7-unknown-linux-musleabihf"
    then "-C link-arg=-lgcc -C link-arg=-static-libgcc"
    else "" end
  ))
} else {
  output: "debug",
  profile: "dev",
  args: ($matrix.debug_build_args // ""),
  features: ($matrix.debug_features // ["rustls", "fancy-with-backtrace"]),
  .rustflags: ($matrix.debug_rustflags // ""),
} end
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
