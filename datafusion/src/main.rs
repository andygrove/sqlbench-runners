use datafusion::common::{DataFusionError, Result};
use datafusion::datasource::MemTable;
use datafusion::prelude::{ParquetReadOptions, SessionConfig, SessionContext};
use datafusion::DATAFUSION_VERSION;
use qpml::from_datafusion;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use structopt::StructOpt;
use tokio::time::Instant;

const TABLES: &[&str] = &[
    "customer", "lineitem", "nation", "orders", "part", "partsupp", "region", "supplier",
];

#[derive(StructOpt, Debug)]
#[structopt(name = "basic")]
struct Opt {
    /// Activate debug mode
    #[structopt(long)]
    debug: bool,

    /// Path to TPC-H queries
    #[structopt(long, parse(from_os_str))]
    query_path: PathBuf,

    /// Path to TPC-H data set
    #[structopt(short, long, parse(from_os_str))]
    data_path: PathBuf,

    /// Output path
    #[structopt(short, long, parse(from_os_str))]
    output: PathBuf,

    /// Query number. If no query number specified then all queries will be executed.
    #[structopt(short, long)]
    query: Option<u8>,

    /// Concurrency
    #[structopt(short, long)]
    concurrency: u8,

    /// Iterations (number of times to run each query)
    #[structopt(short, long)]
    iterations: u8,

    /// Optional GitHub SHA of DataFusion version for inclusion in result yaml file
    #[structopt(short, long)]
    rev: Option<String>,
}

#[derive(Debug, PartialEq, Serialize, Default)]
pub struct Results {
    system_time: u128,
    datafusion_version: String,
    datafusion_github_sha: Option<String>,
    config: HashMap<String, String>,
    command_line_args: Vec<String>,
    register_tables_time: u128,
    /// Vector of (query_number, query_times)
    query_times: Vec<(u8, Vec<u128>)>,
}

impl Results {
    fn new() -> Self {
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards");
        Self {
            system_time: current_time.as_millis(),
            datafusion_version: DATAFUSION_VERSION.to_string(),
            datafusion_github_sha: None,
            config: HashMap::new(),
            command_line_args: vec![],
            register_tables_time: 0,
            query_times: vec![],
        }
    }
}

#[tokio::main]
pub async fn main() -> Result<()> {
    let mut results = Results::new();
    for arg in std::env::args() {
        results.command_line_args.push(arg);
    }

    let opt = Opt::from_args();
    results.datafusion_github_sha = opt.rev;

    let query_path = format!("{}", opt.query_path.display());
    let data_path = format!("{}", opt.data_path.display());
    let output_path = format!("{}", opt.output.display());

    let config = SessionConfig::from_env().with_target_partitions(opt.concurrency as usize);

    for (k, v) in config.config_options.read().options() {
        results.config.insert(k.to_string(), v.to_string());
    }

    let ctx = SessionContext::with_config(config);

    // register tables
    let start = Instant::now();
    for table in TABLES {
        let path = format!("{}/{}.parquet", &data_path, table);
        if Path::new(&path).exists() {
            ctx.register_parquet(table, &path, ParquetReadOptions::default())
                .await?;
        } else {
            return Err(DataFusionError::Execution(format!(
                "Path does not exist: {}",
                path
            )));
        }
    }
    let setup_time = start.elapsed().as_millis();
    println!("Setup time was {} ms", setup_time);
    results.register_tables_time = setup_time;

    match opt.query {
        Some(query) => {
            execute_query(
                &ctx,
                &query_path,
                query,
                opt.debug,
                &output_path,
                opt.iterations,
                &mut results,
            )
            .await?;
        }
        _ => {
            for query in 1..=22 {
                let result = execute_query(
                    &ctx,
                    &query_path,
                    query,
                    opt.debug,
                    &output_path,
                    opt.iterations,
                    &mut results,
                )
                .await;
                match result {
                    Ok(_) => {}
                    Err(e) => println!("Fail: {}", e),
                }
            }
        }
    }

    // write results json file
    let json = serde_json::to_string_pretty(&results).unwrap();
    let f = File::create(&format!("{}/results-{}.yaml", output_path, results.system_time))?;
    let mut w = BufWriter::new(f);
    w.write(json.as_bytes())?;

    Ok(())
}

pub async fn execute_query(
    ctx: &SessionContext,
    query_path: &str,
    query_no: u8,
    debug: bool,
    output_path: &str,
    iterations: u8,
    results: &mut Results,
) -> Result<()> {
    let filename = format!("{}/q{query_no}.sql", query_path);
    println!("Executing query {} from {}", query_no, filename);
    let sql = fs::read_to_string(&filename)?;

    // some queries have multiple statements
    let sql = sql
        .split(';')
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>();

    let multipart = sql.len() > 1;

    let mut durations = vec![];
    for iteration in 0..iterations {
        // duration for executing all queries in the file
        let mut total_duration_millis = 0;

        for (i, sql) in sql.iter().enumerate() {
            if debug {
                println!("Query {}: {}", query_no, sql);
            }

            let file_suffix = if multipart {
                format!("_part_{}", i + 1)
            } else {
                "".to_owned()
            };

            let start = Instant::now();
            let df = ctx.sql(sql).await?;
            let batches = df.collect().await?;
            let duration = start.elapsed();
            total_duration_millis += duration.as_millis();
            println!(
                "Query {}{} executed in: {:?}",
                query_no, file_suffix, duration
            );

            if iteration == 0 {
                let plan = df.to_logical_plan()?;
                let formatted_query_plan = format!("{}", plan.display_indent());
                let filename = format!(
                    "{}/q{}{}_logical_plan.txt",
                    output_path, query_no, file_suffix
                );
                let mut file = File::create(&filename)?;
                write!(file, "{}", formatted_query_plan)?;

                // write QPML
                let qpml = from_datafusion(&plan);
                let filename = format!("{}/q{}{}_logical_plan.qpml", output_path, query_no, file_suffix);
                let file = File::create(&filename)?;
                let mut file = BufWriter::new(file);
                serde_yaml::to_writer(&mut file, &qpml).unwrap();

                // write results to disk
                if batches.is_empty() {
                    println!("Empty result set returned");
                } else {
                    let filename = format!("{}/q{}{}.csv", output_path, query_no, file_suffix);
                    let t = MemTable::try_new(batches[0].schema(), vec![batches])?;
                    let df = ctx.read_table(Arc::new(t))?;
                    df.write_csv(&filename).await?;
                }
            }
        }
        durations.push(total_duration_millis);
    }
    results.query_times.push((query_no, durations));
    Ok(())
}
