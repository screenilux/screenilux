# Contributing to Screenilux

Ideas, bug reports and fixes are all welcome.

- **Found a bug or have an idea?** Open an
  [issue](https://github.com/screenilux/screenilux/issues). For a bug, say
  what you did, what you expected and what happened, and which system you
  use (Windows or Linux, and how many screens).
- **Want to change something?** For anything bigger than a small fix, open an
  issue first so we can agree on the approach before you spend time on it.

## Making a change

1. Install [Rust](https://rustup.rs). On Linux, also install GStreamer's
   development files (`libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev`
   on Ubuntu).
2. Make your change on a branch of your fork.
3. Before opening a pull request, check that these pass:

   ```
   cargo fmt --all --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```

4. Open a pull request into `main`. The checks run on Windows and Linux.

## The Contributor Licence Agreement

Before your first pull request is merged, you agree to the
[Contributor Licence Agreement](CLA.md) once, by posting this comment on your
pull request:

```
I have read the CLA and I agree to it.
```

You keep the copyright in your work. The agreement lets the project offer
Screenilux under the GPL and under other licences, and promises that what
you contribute always stays available as free software. A check called
**CLA** on the pull request shows whether everyone who wrote a commit in it
has agreed.

The check matches commits to GitHub accounts by email address, so make your
commits with an address that is added to your GitHub account.
