+++
title = "Runtime Behavior"
weight = 8
sort_by = "weight"
template = "spec/section.html"
page_template = "spec/page.html"
+++

# Runtime Behavior

This chapter describes runtime behavior in Rue, including error conditions and panics.

{{ rule(id="8.0:1") }}

Certain operations can fail at runtime. These failures are detected and cause the program to terminate with a non-zero exit code.
