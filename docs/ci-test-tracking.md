# Test Runner CI Integration

## Overview

The Rue test runner is fully integrated with GitHub Actions to provide continuous testing, tracking, and visualization of test results over time. This system provides:

- **Automatic test execution** on every push and pull request
- **Historical tracking** of test results and trends
- **Visual dashboards** showing test health over time
- **PR comments** with test result comparisons
- **Regression detection** to catch newly failing tests

## Features

### 1. Automated Test Execution

Every push to trunk and every pull request triggers the test suite:
- Runs all tests in `tests/` and `examples/`
- Validates specification compliance
- Checks for regressions against baseline

### 2. Test Result Tracking

The system maintains historical data:
- **test-baseline.json** - Current baseline on trunk
- **test-history.jsonl** - Historical test results (last 100 runs)
- **Test metrics** - Pass rates, coverage, duration trends

### 3. Visual Dashboard

A dashboard is generated and deployed to GitHub Pages showing:
- Test pass rates over time
- Total test count trends
- Test category distribution
- Current test health metrics

Access the dashboard at: `https://[username].github.io/rue/test-dashboard/test-dashboard.html`

### 4. Pull Request Integration

Every PR receives:
- **Sticky comment** with test results summary
- **Comparison** with trunk baseline
- **Regression warnings** if tests start failing
- **Detailed changes** (new/fixed/removed tests)

### 5. Performance Tracking

The system tracks:
- Test execution duration
- Pass/fail/skip rates
- Specification coverage percentages
- Health scores based on test results

## Workflow Files

### test-runner.yml

Main workflow that:
1. Builds the test runner and compiler
2. Runs the comprehensive test suite
3. Generates metrics and comparisons
4. Posts PR comments
5. Updates baseline on trunk
6. Deploys dashboard to GitHub Pages

### Integration with ci.yml

The main CI workflow includes test runner validation as part of integration tests.

## Scripts

*Note: Test analysis scripts were removed as they were never integrated into CI workflows.*
- Test count over time
- Category distributions
- Interactive visualizations

```bash
python3 scripts/visualize-test-trends.py results.json history.jsonl dashboard.html
```

## PR Comment Format

Pull requests receive comments like:

```markdown
## 🧪 Test Runner Results

### Summary
| Metric | Value |
|--------|-------|
| Total Tests | 19 |
| Passed | 18 |
| Failed | 1 |
| Pass Rate | 94.7% |
| Spec Coverage | 85% |

### Changes from Baseline
📈 Passed: 17 → 18 (+1)
📉 Failed: 0 → 1 (+1)
🔴 1 new failure

✅ No test regressions detected.
```

## Data Storage

Test data is stored in the repository:

- **test-baseline.json** - Current baseline (updated on trunk)
- **test-history.jsonl** - Historical results (appended on trunk)
- **GitHub Pages** - Visual dashboard deployment

## Configuration

### Enable GitHub Pages

1. Go to Settings → Pages
2. Source: Deploy from a branch
3. Branch: gh-pages
4. Folder: / (root)

### Permissions

The workflow requires:
- `contents: write` - Update baseline files
- `pull-requests: write` - Post PR comments
- `pages: write` - Deploy dashboard

## Monitoring Test Health

### Metrics to Watch

1. **Pass Rate** - Should stay above 95%
2. **Spec Coverage** - Should increase over time
3. **Test Count** - Should grow with features
4. **Duration** - Watch for performance regressions

### Failure Patterns

The system tracks common failure reasons:
- Compilation errors
- Runtime failures
- Assertion failures
- Timeout issues

### Regression Detection

Automatic detection of:
- Tests that were passing but now fail
- Significant drops in pass rate
- Coverage decreases
- Performance degradations

## Best Practices

1. **Review PR comments** - Check test results before merging
2. **Fix regressions immediately** - Don't merge with new failures
3. **Add tests with features** - Maintain coverage
4. **Monitor trends** - Check dashboard regularly
5. **Update baselines** - Keep trunk baseline current

## Troubleshooting

### Dashboard Not Updating

1. Check GitHub Pages is enabled
2. Verify gh-pages branch exists
3. Check workflow permissions
4. Review workflow logs

### Baseline Out of Sync

1. Force update with manual workflow run
2. Check git push permissions
3. Verify [skip ci] not blocking updates

### PR Comments Missing

1. Check PR permissions
2. Verify sticky-comment action
3. Check comparison script output

## Future Enhancements

Potential improvements:
- Test flakiness detection
- Performance benchmarking integration
- Code coverage integration
- Test categorization by feature
- Automatic bisection for regressions
- Email/Slack notifications for failures