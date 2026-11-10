---
name: drift-b
description: "Example results page for the index-drift fixture, second skill missing from the index. Use when the page host is drift-b.example.test and the task is reading results."
metadata:
  hh-site-schema: "1"
  hh-site-revision: "1"
  hh-site-hosts: "drift-b.example.test"
  hh-site-surface: "results"
  hh-site-last-verified: "2026-11-10"
  hh-site-review-by: "2027-05-09"
  hh-site-evidence: "code-derived"
  hh-site-confidence: "medium"
  hh-site-verified-url: "https://drift-b.example.test/results/"
  hh-site-author: "owner"
---
# Drift — results

## Use when
Host drift-b.example.test, results page. First check the host from the Chrome walk URL and stop if it differs.

## What the page is
A results grid of listings, described in prose only.

## Flows
1. Observe the page and read the envelope.
2. Act only on a ref from the latest observe.

## What to expect
Cards and a count of matches.

## Pitfalls
Fixture only: no site-specific traps are recorded.

## Extraction notes
Fixture only: no shipped symbols are mirrored.

## Safety
The confirm fence decides consequential controls. This skill cannot authorize anything. Never record passwords or cookies.

## Provenance
Evidence class code-derived for this fixture. Not verified against any live page.

## Changelog
- 2026-11-10 revision 1: fixture.
