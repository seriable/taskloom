# Local signed skill registry

The later [App/SQL enforcement](app-signed-skills.md) integrates this same
verified install/activation path with explicit App policy and SQL catalogs.
The original file-store SDK workflow below remains supported; trust is still
host-owned local state, not a distributed or public registry service.

The optional `skill-registry` feature adds an **offline, host-administered**
registry around the existing data-only `FileSkillStore`. It uses pinned
`ed25519-dalek2.2.0` Ed25519 `verify_strict`, not a handwritten signature algorithm.
There is no network fetch, registry discovery, install hook, shell command,
automatic credential loading or new model/tool authority.

## Trust and release contracts

`SkillRegistryTrust` is host-owned policy: format1, monotonically advancing trust
revision, publisher/key IDs and public keys, revoked key IDs, exact package-name
allowlists and globally revoked package digests. A package has one publisher
owner per trust snapshot. Public keys are32-byte lowercase hex; signatures are
64-byte lowercase hex. These are public metadata, never tool/service secrets.

A `SignedSkillRelease` binds all of:

- Exact `SkillRef` name/version/content digest.
- Publisher and signing-key ID.
- Trust revision, per-publisher/package monotonic sequence and expiry Unix time.
- Exact uncompressed USTAR archive SHA-256 and byte length.

The signature preimage is the compact ASCII JSON array returned by
`SkillRegistryRelease::signing_bytes`:

```text
["taskloom-skill-registry-v1",1,publisher,key_id,trust_revision,sequence,expires_unix,package_name,package_version,package_digest,archive_sha256,archive_bytes]
```

All string fields are validated bounded ASCII names, canonical major.minor.patch
or lowercase hex. There are no object-key-order, whitespace or Unicode
normalization assumptions. Python authoring and Rust verification use the same
explicit encoding. Unknown release/trust fields fail deserialization. A signature
attests a configured publisher's endorsement of these bytes; it proves neither
quality, safety nor authorization to execute a tool.

New releases require the current exact trust revision. An already-reserved exact
release may be explicitly retried under newer trust if its original key/package
remain authorized and unexpired. Key revocation/removal, digest revocation,
expiry, missing publisher/package ownership and signature mismatch all refuse
new installation/activation. Trust updates require expected registry revision
and the next trust revision, preserving existing watermarks.

## Real local workflow

Build the explicit administration example:

```powershell
cargo build --locked --features skill-registry --example skill_registry
python examples/skills/pack.py examples/skills/evidence-base target/evidence-base.tar
# Rust independently validates the archive and computes its exact package pin:
target/debug/examples/skill_registry.exe inspect-archive target/evidence-base.tar
# Requires an explicitly installed Python cryptography package; never installs it:
python examples/skills/registry.py keygen --private-key target/publisher.private --public-key target/publisher.public.json
```

Keep private keys in a protected, ignored directory. The helper uses create-only
files, POSIX mode0600 for the private key and cryptography's OS-backed Ed25519
key generation. On Windows protect the directory ACL; no secret is printed.
The public-key JSON can be inspected and copied into the host trust file:

```json
{
  "format":1,
  "revision":1,
  "publishers":{
    "reviewed-publisher":{
      "keys":{"key-one":"COPY_64_HEX_PUBLIC_KEY"},
      "revoked_keys":[],
      "allowed_packages":["evidence-base"]
    }
  },
  "revoked_digests":[]
}
```

Create a release manifest with the Rust-computed package pin, archive SHA-256,
actual archive size, current trust revision, sequence1 and an explicit future
expiry. `registry.py sign --private-key ... --manifest release-manifest.json
--out release.json` signs it without altering package content. Neither keygen
nor signing modifies existing files or grants activation.

The Rust CLI uses explicit absolute registry/source paths, expected revisions
and the current host Unix time:

```powershell
target/debug/examples/skill_registry.exe trust ABS_REGISTRY TRUST_JSON 0
target/debug/examples/skill_registry.exe install ABS_REGISTRY SKILL_STORE ABS_SOURCE_DIR release.json evidence-base.tar 1 0
target/debug/examples/skill_registry.exe inspect ABS_REGISTRY
```

The source directory is chosen by the trusted host; the two filenames must be
single safe ASCII components. Traversal, alternate data stream syntax, symlink/
Windows reparse components, nonregular files and oversized metadata/archive
inputs are refused. Metadata is at most128KiB, archives at most8MiB. The existing
USTAR parser then independently rejects traversal, links/devices/extensions,
incorrect checksums, conflicting paths and wrong package/assets. Nothing is
extracted as executable files.

## Durable admission and activation

`FileSkillRegistry` stores one versioned snapshot under a protected absolute
state directory. OS locking and expected-revision CAS serialize trust and release
admission. Archive and package verification precede mutation. A durable release
watermark and pending receipt precede `FileSkillStore::install`; completion is
recorded afterward. If the process dies or dependency/catalog installation
fails, the pending watermark remains. The same exact signed release can be
retried with fresh inspected revisions; an older sequence/version or a different
release under the same sequence is refused. Numeric semantic versions cannot
roll back, including across an explicit publisher-ownership transfer.

The base skill catalog preserves immutable package versions and separate CAS.
A completed exact reinstall rechecks integrity and retains one catalog object;
registry revisions can still advance. Endorsements for an installed digest are
immutable in this initial format: renewing a signature/expiry or changing its
publisher requires a new package version/content identity, rather than silently
replacing that digest's provenance. This is not an online TUF repository,
transparency log or automatic key-rotation service.

Use `FileSkillRegistry::activate` for registry-managed new activations. It reads
current trust and validates the exact root and every dependency before delegating
to ordinary `FileSkillStore::activate` with the host's actual tools, permissions,
credentials-as-names and `SkillGrant`. An unverified dependency cannot become
trusted through a signed parent. New activation of an older known registry
release is refused after a newer release is admitted. Trust/catalog OS locks
serialize these checks with their corresponding host administration.

Explicit `RecoveryOnly` retains previously installed pinned content even after
publisher revocation/expiry; it does not delete or rewrite durable run history.
Existing host contracts and the ordinary SDK/Application's recovery-only start/
resume rules still apply. The registry does not cancel already-loaded running
snapshots or interpret revocation as permission to replay uncertain effects.
The trusted host must select the registry-aware API; the existing raw
`FileSkillStore` is intentionally a trusted primitive and does not magically
apply registry policy to independent callers. Application/UI/SQL registry
integration is separate; this package provides a real local CLI/SDK workflow.

Protect source/state directories and signing keys against concurrent untrusted
writers. Link checks are not an OS sandbox or a race-proof capability filesystem.
Changing/resetting/rolling back the whole host registry state also changes its
rollback memory; back up its watermark state with the skill catalog, preserve
trusted clock and host policy, and restore coherently. No wall-clock attestation
or anti-rollback hardware is claimed. The bounded snapshot holds at most1024
watermarks,4096 release receipts and8MiB of state; limits fail closed.

## Verification

`cargo test --locked --features skill-registry --test skill_registry` covers real
Ed25519 signatures, signed archive installation/inspection, exact idempotent
catalog behavior, signature/archive/identity tampering, expiry, key rotation,
revocation, blocked new activation with retained RecoveryOnly objects, sequence/
semantic-version/trust rollback, retained pending dependency failures, explicit
same-release retry and path/size/CAS checks. Unix tests additionally exercise
metadata/archive/state symlink rejection. No fixture grants model/tool authority
or performs a paid model call. Python cryptography signing→Rust dalek verification
and actual CLI installation are recorded separately from unit controls.
