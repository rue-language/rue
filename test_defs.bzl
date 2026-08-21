"""Shared metadata wrappers for Rue's first-party Buck test targets."""

TEST_TIER_PREMERGE = "rue_test_tier_premerge"
TEST_TIER_SLOW = "rue_test_tier_slow"
TEST_TIER_STRESS = "rue_test_tier_stress"
PLATFORM_NATIVE = "rue_platform_native"

# The one declaration of the tier vocabulary. test_tiers.bxl loads this list
# instead of keeping its own, so the selector and the macros cannot disagree;
# scripts/validate-tier-ci-selectors.py reads it from this file only.
RUE_TEST_TIER_LABELS = [
    TEST_TIER_PREMERGE,
    TEST_TIER_SLOW,
    TEST_TIER_STRESS,
]

# Platform scope is deliberately a small, validated vocabulary just like test
# tier.  Consumers query this label from the live Buck graph; they must not
# maintain a second list of native targets in workflow or policy files.
RUE_PLATFORM_SCOPE_LABELS = [PLATFORM_NATIVE]

_TEST_TIER_LABELS = {
    "premerge": TEST_TIER_PREMERGE,
    "slow": TEST_TIER_SLOW,
    "stress": TEST_TIER_STRESS,
}

def rue_test_labels(tier, platform = None, labels = []):
    """Returns labels with one validated tier and optional platform scope."""
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

    caller_platforms = [
        label
        for label in labels
        if label in RUE_PLATFORM_SCOPE_LABELS
    ]
    if caller_platforms:
        fail("pass platform = instead of adding Rue platform labels directly: {}".format(
            caller_platforms,
        ))

    if platform not in [None, "native"]:
        fail("unknown Rue platform scope '{}'; expected 'native' or None".format(platform))

    return labels + [_TEST_TIER_LABELS[tier]] + ([PLATFORM_NATIVE] if platform == "native" else [])

def rue_sh_test(name, tier = "premerge", platform = None, labels = [], **kwargs):
    """Defines a sh_test with explicit Rue execution-tier ownership."""
    native.sh_test(
        name = name,
        labels = rue_test_labels(tier, platform, labels),
        **kwargs
    )

def rue_rust_test(name, tier = "premerge", platform = None, labels = [], **kwargs):
    """Defines a rust_test with explicit Rue execution-tier ownership."""
    native.rust_test(
        name = name,
        labels = rue_test_labels(tier, platform, labels),
        **kwargs
    )

def rue_test_suite(name, tier = "premerge", platform = None, labels = [], **kwargs):
    """Defines a test_suite with explicit Rue execution-tier ownership."""
    native.test_suite(
        name = name,
        labels = rue_test_labels(tier, platform, labels),
        **kwargs
    )
