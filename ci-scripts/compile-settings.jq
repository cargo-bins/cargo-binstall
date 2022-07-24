if $for_release then {
  output: "release",
  profile: "release",
  args: ($matrix.release_build_args // ""),
} else {
  output: "debug",
  profile: "dev",
  args: ($matrix.debug_build_args // "--no-default-features --features rustls"),
} end
|
{
  CBIN: (if ($matrix.target | test("windows")) then "cargo-binstall.exe" else "cargo-binstall" end),
  CTOOL: (if ($matrix."use-cross" // false) then "cross" else "cargo" end),
  COUTPUT: .output,
  CARGS: "--target \($matrix.target) --profile \(.profile) \(.args)",
}
|
to_entries[] | "\(.key)=\(.value)"
