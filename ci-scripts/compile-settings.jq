if ($ref | startswith("refs/tags/v")) then {
  output: "release",
  profile: "release",
  args: ($matrix.release_build_args // ""),
} else {
  output: "debug",
  profile: "dev",
  args: ($matrix.debug_build_args // ""),
} end
|
{
  CTOOL: (if $matrix."use-cross" then "cross" else "cargo" end),
  COUTPUT: .output,
  CARGS: "--target \($matrix.target) --profile \(.profile) \(.args)",
}
|
to_entries[] | "\(.key)=\(.value)"
