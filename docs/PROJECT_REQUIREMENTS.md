# GOAL

"VIM Design" is a high-performance Rust library which is callable from C++ which lets the caller submit undoable commands to generate the BREP geometry which composes a building. It must never crash. It can be used in a web browser application to create BREP building geometry.

# ARCHITECTURE

The API lets the caller submit undoable commands similar to the following set:
- "CreateControlPoint" (a movable control point in space; may be a terminal point on a spline or a control point for the spline)
- "UpdateControlPoint"
- "DeleteControlPoint" (note: deletion commands may not always be possible depending on the dependencies among entities)
- "CreatePlane"
- "UpdatePlane"
- "DeletePlane"
- "CreateCircle"
- "UpdateCircle"
- "DeleteCircle"
- "CreateLine"
- "UpdateLine"
- "DeleteLine"
- "CreateSpline"
- "UpdateSpline"
- "DeleteSpline"
- "CreateEdge"
- "UpdateEdge"
- "DeleteEdge"
- "CreateWire" (an ordered, closed loop of edges — the boundary of a face)
- "UpdateWire"
- "DeleteWire"
- "CreateFace" (from one outer wire plus optional inner hole wires and an optional plane; a planar surface is inferred from the wire when no plane is specified)
- "UpdateFace"
- "DeleteFace"
- "CreateSolid" (from face(s))
- "UpdateSolid"
- "DeleteSolid"
- "CreateMaterial"
- "UpdateFaceMaterial"
- "DeleteMaterial"
- "CreateExtrusion" (from face and spline or line)
- "UpdateExtrusion"
- "DeleteExtrusion"
- "CreateChamfer" (from faces and edges)
- "UpdateChamfer"
- "DeleteChamfer"
- "CreateSectionBox" ("cuts" the solids based on the planes of the section box)
- "UpdateSectionBox"
- "DeleteSectionBox"
- "CreateElement" (groups a construction graph of entities producing solid(s) — the reusable definition)
- "UpdateElement"
- "DeleteElement"
- "CreateInstance" (places an Element in the scene at a transform; many instances share one element's evaluated geometry)
- "UpdateInstance"
- "DeleteInstance"

A "VimDesign" object accumulates the state of the entities as commands are submitted. Multiple "VimDesign" objects may be created. A "VimDesign" object contains:
- The command stack allowing the caller to "UndoCommand" or "RedoCommand".
- A facade allowing the caller to evaluate the triangular mesh geometry to present to a graphics API

Entities like control points, planes, splines, edges, faces, solids, materials, etc are connected via a "DependencyGraph" (a directed acyclic graph) so that modifying one entity upstream affects all related entities downstream

Shorthand "composite" commands are available in the API like "CreateCylinder", "UpdateCylinder", and "DeleteCylinder" which groups a bunch of commands to generate a cylinder.

# TODO

## Create docs/ARCHITECTURE.md to flesh out VIM Design's architecture in detail to guide the implementation

## Set up dependencies
Use "truck" or any other modern and maintained Rust cargo packages to avoid re-implementing some things from scratch

## Implement Rust build system
- VimDesignLib: Rust library
- VimDesignWeb: Rust WebGPU + WASM application which uses VimDesignLib to let the user test interactively in a browser
- VimDesignTest: Rust unit & integration testing application
- VimDesignWebTest: Playwright-driven tests with screenshots to validate the implementation of VimDesignWeb.

## Implement C++ build system
- VimDesignCppTest: C++ google test application to validate that C++ can be used to successfully call into VimDesignLib.

## Implement automated testing mechanism
- VimDesignTest: runs and validates the VimDesignLib implementation, including: command system, serialization, outputs, and contains regression tests to avoid recurring errors.
- VimDesignCppTest: runs and validates that C++ can correctly call into VimDesignLib.
- VimDesignWebTest: runs and validates the Playwright-driven tests to ensure the Rust WebGPU + WASM application runs as expected.

## Devops scripts
- ./devops/vbuild.ps1 - powershell script to build the whole codebase and pull/install Rust and C++ (CMake) dependencies if needed.
  "vbuild.ps1 -Clean" -> cleans the repository of untracked files
  "vbuild.ps1 -Debug" -> builds the Rust and C++ parts in debug mode
  "vbuild.ps1 -Release" -> builds the Rust and C++ parts in release mode
  "vbuild.ps1 -Clean -Release" -> cleans the repository the builds the Rust and C++ parts in release mode
- ./devops/vactions.ps1 - powershell script to run actions:
  "vactions.ps1 -VimDesignWeb" -> Opens a browser and runs VimDesignWeb for interactive usage/testing by the user
  "vactions.ps1 -Test" -> Sequentially runs all tests.
- ./devops/lib/*.ps1 - utility functions to keep the top-level vbuild.ps1 and vactions.ps1 implementations simple

