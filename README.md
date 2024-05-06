# App-Queue

App-queue is a simple persistent in-app queue for Rust. It is designed to be dropped into monolithic applications, and provide automatic queueing and retrying for asynchronous tasks.

Additionally, it allows the use of any serializable data type for requests.

By default, if a job returns an error, it will be retried indefinitely with exponential backoff (600s max delay). This can however be overridden on a per-job-type basis.

Sample usage, from the documentation:

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MyJob {
    message: String
}

#[typetag::serde]
#[async_trait::async_trait]
impl Job for MyJob {
  async fn run(&mut self, _: Arc<AppQueue>) -> Result<()> {
    println!("{}", self.message);
    Ok(())
  }
}

#[tokio::main]
async fn main() -> Result<()> {
# tracing_subscriber::fmt::init();
  let queue = AppQueue::new("/tmp/queue.db").await?;
  let job = MyJob {
    message: "Hello, world!".into()
  };
  queue.add_job(Box::new(job)).await?;
  queue.run_job_workers_default();
  Ok(())
}
```

A larger example [can be found here](https://github.com/DarkKirb/attic/blob/main/queue/src/main.rs).

## Caveats

Incompatible schema changes (Renaming/Removal of Job structs), as well as incompatible changes in the structure of the job itself will cause jobs to get stuck in the queue. This will cause errors to show up on startup, however they will be ignored for the rest of the program’s runtime.

## Limitations

Potentially desirable features that are currently not supported:

- Integration with the app’s storage. The app likely already uses a database, and it would be useful to only have one database total. Proper DBMSes may also improve performance in some cases.
