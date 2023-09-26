# Signature support

Binstall supports verifying signatures of downloaded files.
At the moment, only one algorithm is supported, but this is expected to improve as time goes.

This feature requires adding to the Cargo.toml metadata: no autodiscovery here!

## Minimal example

Generate a [minisign](https://jedisct1.github.io/minisign/) keypair:

```console
minisign -G -W -p signing.pub -s signing.key

# or with rsign2:
rsign generate -W -p signing.pub -s signing.key
```

In your Cargo.toml, put:

```toml
[package.metadata.binstall.signing]
algorithm = "minisign"
pubkey = "RWRnmBcLmQbXVcEPWo2OOKMI36kki4GiI7gcBgIaPLwvxe14Wtxm9acX"
```

Replace the value of `pubkey` with the public key in your `signing.pub`.

Save the `signing.key` as a secret in your CI, then use it when building packages:

```console
tar cvf package-name.tar.zst your-files # or however

minisign -S -W -s signing.key -x package-name.tar.zst.sig -m package-name.tar.zst

# or with rsign2:
rsign sign -W -s signing.key -x package-name.tar.zst.sig package-name.tar.zst
```

Upload both your package and the matching `.sig`.

Now when binstall downloads your packages, it will also download the `.sig` file and use the `pubkey` in the Cargo.toml to verify the signature.
If the signature has a trusted comment, it will print it at install time.

By default, `minisign` and `rsign2` prompt for a password; above we disable this with `-W`.
While you _can_ set a password, we recommend instead using [age](https://github.com/FiloSottile/age) (or the Rust version [rage](https://github.com/str4d/rage)) to separately encrypt the key, which we find is much better for automation.

```console
rage-keygen -o age.key
Public key: age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p

rage -r age1ql3z7hjy54pw3hyww5ayyfg7zqgvc7w3j2elw8zmrj2kg5sfn9aqmcac8p -o signing.key.age signing.key
rage -d -i age.key -o signing.key signing.key.age
```

For just-in-time or "keyless" schemes, securely generating and passing the ephemeral key to other jobs or workflows presents subtle issues.
`cargo-binstall` has an implementation in [its own release process][`release.yml`] that you can use as example.

[`expect`]: https://linux.die.net/man/1/expect
[`release.yml`]: https://github.com/cargo-bins/cargo-binstall/blob/main/.github/workflows/release.yml

## Reference

- `algorithm`: required, see below.
- `pubkey`: required, must be the public key.
- `file`: optional, a template to specify the URL of the signature file. Defaults to `{ url }.sig` where `{ url }` is the download URL of the package.

### Minisign

`algorithm` must be `"minisign"`.

The legacy signature format is not supported.

The `pubkey` must be in the same format as minisign generates.
It may or may not include the untrusted comment; it's ignored by Binstall so we recommend not.

## Just-in-time signing

To reduce the risk of a key being stolen, this scheme supports just-in-time or "keyless" signing.
The idea is to generate a keypair when releasing, use it for signing the packages, save the key in the Cargo.toml before publishing to a registry, and then discard the private key when it's done.
That way, there's no key to steal nor to store securely, and every release is signed by a different key.
And because crates.io is immutable, it's impossible to overwrite the key.

There is one caveat to keep in mind: with the scheme as described above, Binstalling with `--git` may not work:

- If the Cargo.toml in the source contains a partially-filled `[...signing]` section, Binstall will fail.
- If the section contains a different key than the ephemeral one used to sign the packages, Binstall will refuse to install what it sees as corrupt packages.
- If the section is missing entirely, Binstall will work, but of course signatures won't be checked.

The solution here is either:

- Commit the Cargo.toml with the ephemeral public key to the repo when publishing.
- Omit the `[...signing]` section in the source, and write the entire section on publish instead of just filling in the `pubkey`; signatures won't be checked for `--git` installs. Binstall uses this approach.
- Instruct your users to use `--skip-signatures` if they want to install with `--git`.

## Why not X? (Sigstore, GPG, signify, with SSH keys, ...)

We're open to pull requests adding algorithms!
We're especially interested in Sigstore for a better implementation of "just-in-time" signing (which it calls "keyless").
We chose minisign as the first supported algorithm as it's lightweight, fairly popular, and has zero options to choose from.

## There's a competing project that does package signature verification differently!

[Tell us about it](https://github.com/cargo-bins/cargo-binstall/issues/1)!
We're not looking to fracture the ecosystem here, and will gladly implement support if something exists already.

We'll also work with others in the space to eventually formalise this beyond Binstall, for example around the [`dist-manifest.json`](https://crates.io/crates/cargo-dist-schema) metadata format.

## What's the relationship to crate/registry signing?

There isn't one.
Crate signing is something we're also interested in, and if/when it materialises we'll add support in Binstall for the bits that concern us, but by nature package signing is not related to (source) crate signing.
