# A writer for object file ar archives

This is a Rust port of LLVM's archive writer (`ArchiveWriter.cpp`):
* Based on commit [8ef3e895a](https://github.com/llvm/llvm-project/tree/3d3ef9d073e1e27ea57480b371b7f5a9f5642ed2) (15.0.0-rc3).
* With the following options removed:
  * Deterministic: always enabled.
  * Symbol tables: always enabled.

## License

Licensed under Apache License v2.0 with LLVM Exceptions
([LICENSE.txt](LICENSE.txt) or https://llvm.org/LICENSE.txt)

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you shall be dual licensed as above, without any
additional terms or conditions.
