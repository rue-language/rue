"""Shared metadata wrappers for Rue's first-party Buck test targets."""

TEST_TIER_PREMERGE = "rue_test_tier_premerge"
TEST_TIER_SLOW = "rue_test_tier_slow"
TEST_TIER_STRESS = "rue_test_tier_stress"

# The one declaration of the tier vocabulary. test_tiers.bxl loads this list
# instead of keeping its own, so the selector and the macros cannot disagree;
# scripts/validate-tier-ci-selectors.py reads it from this file only.
RUE_TEST_TIER_LABELS = [
    TEST_TIER_PREMERGE,
    TEST_TIER_SLOW,
    TEST_TIER_STRESS,
]

_TEST_TIER_LABELS = {
    "premerge": TEST_TIER_PREMERGE,
    "slow": TEST_TIER_SLOW,
    "stress": TEST_TIER_STRESS,
}

def rue_test_labels(tier, labels = []):
    """Returns labels with exactly one validated Rue execution tier."""
    if tier not in _TEST_TIER_LABELS:
        fail("unknown Rue test tier '{}'; expected one of {}".format(
            tier,
            sorted(_TEST_TIER_LABELS.keys()),
        ))

    caller_tiers = [
        label
        for label in labels
        if label in _TEST_TIER_LABELS.values()
    ]
    if caller_tiers:
        fail("pass tier = instead of adding Rue test-tier labels directly: {}".format(
            caller_tiers,
        ))

    return labels + [_TEST_TIER_LABELS[tier]]

def rue_sh_test(name, tier = "premerge", labels = [], **kwargs):
    """Defines a sh_test with explicit Rue execution-tier ownership."""
    native.sh_test(
        name = name,
        labels = rue_test_labels(tier, labels),
        **kwargs
    )

def rue_rust_test(name, tier = "premerge", labels = [], **kwargs):
    """Defines a rust_test with explicit Rue execution-tier ownership."""
    native.rust_test(
        name = name,
        labels = rue_test_labels(tier, labels),
        **kwargs
    )

def rue_test_suite(name, tier = "premerge", labels = [], **kwargs):
    """Defines a test_suite with explicit Rue execution-tier ownership."""
    native.test_suite(
        name = name,
        labels = rue_test_labels(tier, labels),
        **kwargs
    )
