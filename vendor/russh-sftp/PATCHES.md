# fesTerm's russh-sftp patch

Source: Apache-2.0 `russh-sftp` 2.3.0, published by
<https://github.com/AspectUnk/russh-sftp>. The original `LICENSE`, manifest,
source and package provenance are retained.

The crates.io archive SHA-256 is
`9ed8949eca4163c18a8f59ff96d32cf61e9c13b9735e21ef32b3907f4aafa1a9`.
It was verified before extraction.

The only source change adds `SftpSession::raw_session`, returning a clone of
the existing `Arc<RawSftpSession>`. fesTerm uses its existing `opendir`,
`readdir` and `close` operations for budgeted recursive planning. The upstream
convenience `read_dir` collects the entire directory and recopies prior pages
before returning, so it cannot enforce admission during enumeration.
Ordinary upstream convenience APIs and connection ownership are unchanged.

The getter is exercised by fesTerm's in-process SFTP server tests. Keep this
patch narrow; re-check the upstream API before updating the pinned dependency.
