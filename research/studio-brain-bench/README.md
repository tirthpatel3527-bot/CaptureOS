# StudioBrainBench

`studio-brain-bench` is a deterministic, generated-data measurement harness for Studio Brain I mechanics. It uses the product `studio-brain` crate and never opens a CaptureOS catalog, AI Test project, original, preview, embedding, model weight, or network connection.

Run the baseline:

```sh
cargo run -p studio-brain-bench -- --suite baseline
```

It reports JSON for 100, 1k, 10k, 50k, and 100k generated observations: generation/training/inference timings, estimated input lower-bound memory, serialized model bytes, readiness, grouped holdout metrics, pairwise-ranker availability/timing, and a fixed Studio A versus Studio B probe. Use `--records 100,1000` for a focused scale set. `--output NEW_REPORT_PATH` creates a new report and deliberately refuses overwrite.

## What it evaluates

- Generic technical and majority-class baselines against the generated human-decision rule.
- A regularized three-class local linear model on deterministic project-grouped holdouts.
- Calibration/abstention behavior emitted by the product model.
- Separate Similar Set pairwise-ranking mechanics from generated explicit representative choices.
- Whether two deliberately different synthetic profiles can yield different advisory outcomes for the same probe.

## What it does not prove

The synthetic labels encode controlled preference rules. Accuracy, macro F1, Brier score, agreement delta, and pairwise behavior therefore measure recovery of the generator—not creative quality, photographer psychology, real-world usefulness, identity, emotion, genre generalization, or permission to auto-cull. Timings are local-machine observations, not performance guarantees. SQLite migrations, profile/exclusion policy, activation/rollback, UI behavior, and privacy boundaries are covered by product tests rather than this benchmark.

Any real-photographer research requires separate explicit consent, privacy, licensing, data-governance, and evaluation approval. Never commit customer media, private decisions, notes, filenames, paths, profile artifacts, or raw embeddings here.
