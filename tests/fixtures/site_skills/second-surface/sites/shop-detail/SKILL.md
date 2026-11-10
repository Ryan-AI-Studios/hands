---
name: shop-detail
description: "Example shop vehicle detail page for the fixture: the single listing view. Use when the page host is shop.example.test and the task is reading one vehicle."
metadata:
  hh-site-schema: "1"
  hh-site-revision: "1"
  hh-site-hosts: "shop.example.test"
  hh-site-surface: "vehicle-detail"
  hh-site-last-verified: "2026-11-10"
  hh-site-review-by: "2027-05-09"
  hh-site-evidence: "code-derived"
  hh-site-confidence: "medium"
  hh-site-verified-url: "https://shop.example.test/vehicle/"
  hh-site-author: "owner"
---
# Shop — vehicle detail

## Use when
Host shop.example.test, single vehicle page. First check the host from the Chrome walk URL and stop if it differs.

## What the page is
One listing with its details, described in prose only.

## Flows
1. Observe the page and read the envelope.
2. Act only on a ref from the latest observe.

## What to expect
Labelled facts for one vehicle.

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
