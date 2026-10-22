// API 3 uses the sole-origin encrypted transport. Both two-account and
// three-computer fixtures exercise the same real UI and fixed-grant protocol.
// Legacy domain-origin fixtures are deliberately rejected, never downgraded.
require('./relay-single-origin-probe.cjs');
