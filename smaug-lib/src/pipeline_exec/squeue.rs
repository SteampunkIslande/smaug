use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::RwLock;
use tokio::time::Instant;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobStatus {
    pub job_id: String,
    pub job_name: String,
    pub state: String,
    pub elapsed: String,
    pub time_limit: String,
    pub nodes: String,
    pub nodelist: String,
    pub reason: String,
}

/// Result of a squeue lookup.
///
/// A failure is a first-class cached outcome: unlike a missing cache entry, it
/// has a (short) TTL and therefore resolves itself on a later poll.
#[derive(Clone, Debug)]
pub enum SqueueSnapshot {
    NoJobName,
    Available {
        job_name: String,
        jobs: Vec<JobStatus>,
    },
    Unavailable {
        job_name: String,
        message: String,
    },
}

struct CacheEntry {
    job_name: String,
    snapshot: SqueueSnapshot,
    fetched_at: Instant,
    ttl: Duration,
}

/// Per-attempt cache of `squeue` answers.
///
/// It is deliberately keyed by `(run_id, attempt_number)`, not by job name:
/// this is the identifier the rest of the application uses to follow a given
/// analysis. The job name is remembered in the entry so a new workflow (a new
/// `SLURM run ID`) invalidates the previous answer automatically.
pub struct SqueueCache {
    entries: RwLock<HashMap<(i64, u32), CacheEntry>>,
    ttl: Duration,
    failure_ttl: Duration,
}

impl SqueueCache {
    pub fn new(ttl_seconds: u64) -> Self {
        let ttl = Duration::from_secs(ttl_seconds);
        Self {
            entries: RwLock::new(HashMap::new()),
            ttl,
            // Retry a failing `squeue` sooner than a successful query, but never
            // on every HTTP poll.
            failure_ttl: ttl.min(Duration::from_secs(5)),
        }
    }

    pub async fn get(&self, run_id: i64, attempt_number: u32, job_name: &str) -> SqueueSnapshot {
        let job_name = job_name.trim();
        if job_name.is_empty() {
            return SqueueSnapshot::NoJobName;
        }

        let key = (run_id, attempt_number);
        {
            let entries = self.entries.read().await;
            if let Some(entry) = entries.get(&key)
                && entry.job_name == job_name
                && entry.fetched_at.elapsed() < entry.ttl
            {
                return entry.snapshot.clone();
            }
        }

        let snapshot = match fetch_squeue_from_slurm(job_name).await {
            Ok(jobs) => SqueueSnapshot::Available {
                job_name: job_name.to_string(),
                jobs,
            },
            Err(message) => SqueueSnapshot::Unavailable {
                job_name: job_name.to_string(),
                message,
            },
        };

        let ttl = match &snapshot {
            SqueueSnapshot::Unavailable { .. } => self.failure_ttl,
            _ => self.ttl,
        };

        let mut entries = self.entries.write().await;
        entries.retain(|_, entry| entry.fetched_at.elapsed() < entry.ttl);
        entries.insert(
            key,
            CacheEntry {
                job_name: job_name.to_string(),
                snapshot: snapshot.clone(),
                fetched_at: Instant::now(),
                ttl,
            },
        );

        snapshot
    }

    pub async fn invalidate(&self, run_id: i64, attempt_number: u32) {
        self.entries.write().await.remove(&(run_id, attempt_number));
    }
}

pub async fn fetch_squeue_from_slurm(job_name: &str) -> Result<Vec<JobStatus>, String> {
    let output = match tokio::time::timeout(
        Duration::from_secs(5),
        Command::new("squeue")
            .args([
                "-h",
                "-n",
                job_name,
                "-o",
                "%.20i|%.80j|%.20T|%.20M|%.20l|%.10D|%.80N|%.80r",
            ])
            .output(),
    )
    .await
    {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(format!("Impossible de lancer squeue : {e}")),
        Err(_) => return Err("squeue n’a pas répondu dans les 5 secondes".to_string()),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        return Err(if stderr.is_empty() {
            format!("squeue a échoué ({})", output.status)
        } else {
            format!("squeue a échoué ({}): {stderr}", output.status)
        });
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| format!("La sortie de squeue n’est pas de l’UTF-8 valide : {e}"))?;

    Ok(stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(parse_job_line)
        .collect())
}

fn parse_job_line(line: &str) -> Option<JobStatus> {
    let mut parts = line.splitn(8, '|');

    Some(JobStatus {
        job_id: parts.next()?.trim().to_string(),
        job_name: parts.next()?.trim().to_string(),
        state: parts.next()?.trim().to_string(),
        elapsed: parts.next()?.trim().to_string(),
        time_limit: parts.next()?.trim().to_string(),
        nodes: parts.next()?.trim().to_string(),
        nodelist: parts.next()?.trim().to_string(),
        reason: parts.next().unwrap_or_default().trim().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pipe_separated_squeue_line() {
        let job =
            parse_job_line("12345|pipeline-job|RUNNING|00:12:34|1-00:00:00|2|node[01-02]|None")
                .unwrap();

        assert_eq!(job.job_id, "12345");
        assert_eq!(job.job_name, "pipeline-job");
        assert_eq!(job.state, "RUNNING");
        assert_eq!(job.elapsed, "00:12:34");
        assert_eq!(job.time_limit, "1-00:00:00");
        assert_eq!(job.nodes, "2");
        assert_eq!(job.nodelist, "node[01-02]");
        assert_eq!(job.reason, "None");
    }

    #[test]
    fn rejects_incomplete_squeue_line() {
        assert!(parse_job_line("12345|pipeline-job|RUNNING").is_none());
    }

    #[tokio::test]
    async fn empty_job_name_is_answered_without_querying_squeue() {
        let cache = SqueueCache::new(30);

        assert!(matches!(
            cache.get(1, 1, "   ").await,
            SqueueSnapshot::NoJobName
        ));
    }
}
