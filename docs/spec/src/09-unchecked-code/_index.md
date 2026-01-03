+++
title = "Unchecked Code"
weight = 9
sort_by = "weight"
template = "spec/section.html"
page_template = "spec/page.html"
+++

# Unchecked Code

This chapter describes Rue's mechanism for low-level operations that bypass normal safety checks.

{{ rule(id="9.0:1") }}

Rue provides `checked` blocks and `unchecked` functions to enable low-level memory operations while keeping such code visibly separate from normal safe code.
