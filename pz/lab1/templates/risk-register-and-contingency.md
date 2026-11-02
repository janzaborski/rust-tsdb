# Risk Register & Contingency Plans

**Engineering / Master's Degree Project – Faculty of MIS, WUT**

---

## 1. Project and Technical Risk Register

|     ID     | Risk Event                                             | Likelihood (L/M/H) | Impact (L/M/H/Crit) |  Owner   | Mitigation Strategy                                                                                                                 |
| :--------: | :----------------------------------------------------- | :----------------: | :-----------------: | :------: | :---------------------------------------------------------------------------------------------------------------------------------- |
| **RSK-01** | **Team member illness / drop-out**                     |         M          |          H          | Alice K. | Peer code review for every PR, pair programming on the critical path, modular architecture with clear API contracts.                |
| **RSK-02** | **External API unavailability or rate limits**         |         H          |          H          |  Bob N.  | Implementation of an internal mock/stub server simulating data streams; local response caching.                                     |
| **RSK-03** | **Insufficient ML model accuracy**                     |         M          |          H          | David Z. | Immediate implementation of a simple baseline model (heuristics / simple baseline); result consultation with supervisor by week 6.  |
| **RSK-04** | **Environment incompatibility in the MiNI laboratory** |         H          |        Crit         | David Z. | Full containerization using Docker Compose; test deployment on lab machines during Stages 2 and 4, not the night before acceptance. |
| **RSK-05** | **Hardware failure (loss of data / code on laptop)**   |         M          |        Crit         | Everyone | Daily pushes to remote repository (GitLab/GitHub); versioning of models and datasets with SHA256 checksums.                         |
| **RSK-06** | **Thesis writing workload accumulation in January**    |         H          |          H          | Alice K. | Incremental drafting of thesis chapters (Chapters 1–3 finished by week 11); peer reviews within the team.                           |
| **RSK-07** | **Data leakage in experiments**                        |         M          |          H          | David Z. | Isolating the test dataset before any preprocessing; utilizing Pipeline objects in scikit-learn.                                    |

---

## 2. Contingency Plans for High-Risk Modules (Plan B Protocols)

### Contingency Protocol 1: Unavailability of GPU Compute Cluster

- **Plan A (Primary):** Train a deep neural network (Time-series Transformer) on the faculty MiNI GPU cluster.
- **Plan B Trigger:** Lack of compute resource allocation or queue wait time $> 7$ days by week 9.
- **Plan B (Contingency):** Deploy an optimized, quantized XGBoost / LightGBM model with feature engineering that trains in $< 10$ minutes on a standard laptop CPU and meets MVP criteria.

### Contingency Protocol 2: Unavailability of Real-World Industrial Dataset

- **Plan A (Primary):** Acquire production logs from an external industrial partner (under NDA).
- **Plan B Trigger:** Legal consent / NDA not signed by the company by week 4 of the semester.
- **Plan B (Contingency):** Utilize open reference datasets (NAB – Numenta Anomaly Benchmark and NASA Turbofan benchmarks) and generate synthetic anomalies using a dedicated generator.

### Contingency Protocol 3: Live Demo Failure during Defense

- **Plan A (Primary):** Run the application live inside Docker containers during the 10-minute presentation.
- **Plan B Trigger:** Projector connectivity issues, network failure, or database startup lag.
- **Plan B (Contingency):** Immediately switch to playback of a pre-recorded, high-resolution 60-second video demo (screencast) narrated live by a team member.
