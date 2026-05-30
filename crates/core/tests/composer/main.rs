// Consolidated integration-test binary: groups several former top-level
// tests/*.rs files into one target to cut link time. Each module is a
// verbatim relocation; #[test] behavior is unchanged.
mod compose_inheritance;
mod composer_adversarial;
mod composer_cross_omics;
mod composer_cross_omics_n_way;
mod composer_determinism;
mod composer_generic_omics_fallthrough;
mod composer_literature;
mod composer_offline;
