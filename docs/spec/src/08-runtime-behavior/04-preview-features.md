+++
title = "Preview Features"
weight = 4
+++

# Preview Features

Preview features are in-progress language additions that require explicit opt-in.

{{ rule(id="8.4:1", cat="normative") }}

Features gated behind a preview flag produce a compile-time error when used without the corresponding `--preview <feature>` flag. The error message includes the required flag name.
