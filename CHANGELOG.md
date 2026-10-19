## Unreleased

### Bug Fixes

* Limit passive details-popup damage repair to exposed cells instead of repainting the entire window.
* Avoid redundant popup frames for unchanged pointer hover, owner updates and scroll limits.
* Keep normal provider refresh progress out of red error messages while retaining real failures during retries.
* Show CI for the corresponding remote branch head even when local commits are unpushed, with explicit checked-ref and commit labels.

### Maintenance

* Build the pinned `calops/bkpr-zellij` fork instead of applying a local patch queue; retain native features on separate branches and keep release caching in Bkpr.

## [0.1.1](https://github.com/calops/bkpr/compare/v0.1.0...v0.1.1) (2026-10-16)

### Bug Fixes

* **nix:** include bundled OMP extension in package source ([627e416](https://github.com/calops/bkpr/commit/627e41636d388bf30461cec488b134cf41493f2e))
