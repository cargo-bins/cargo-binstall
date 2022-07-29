if $for_release then {
  output: "release",
  profile: "release",
  args: ($matrix.release_build_args // ""),
  features: ($matrix.release_features // []),
} else {
  output: "debug",
  profile: "dev",
  args: ($matrix.debug_build_args // ""),
  features: ($matrix.debug_features // ["rustls", "fancy-with-backtrace"]),
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
}
|
to_entries[] | "\(.key)=\(.value)"
