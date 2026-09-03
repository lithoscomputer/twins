//! Offline replay of the recorded contract fixtures through the twin.
//!
//! Boots the twin in-process with `fixtures/scenarios.json` in strict
//! fixture mode, replays every registry case, canonicalizes each exchange
//! with the same functions as the live suite, and asserts the same named
//! snapshots. Every CI run therefore re-proves, without network access, that
//! the twin reproduces the last captured contract.
//!
//! Do not accept snapshot changes from this suite: the snapshots record the
//! live API's canonical behavior, and `mise run record` is the only blessed
//! write path. A failure here after re-recording means the twin cannot
//! reproduce the new live shape.

mod common;

use std::collections::HashSet;

use anyhow::{Context, Result};
use serde_json::Value;

use common::cases::ContractCase;
use common::normalize::CanonicalExchange;

#[tokio::test]
async fn replay_contract_snapshots() {
    let scenarios_path = common::fixtures::scenarios_path();
    let raw = std::fs::read_to_string(&scenarios_path).unwrap_or_else(|error| {
        panic!(
            "scenario fixtures missing at {} ({error}); run `mise run record` to create them",
            scenarios_path.display()
        )
    });
    let scenario_ids = scenario_ids(&raw);

    let server = common::spawn_server_with_scenarios(Some(scenarios_path))
        .await
        .expect("server should spawn");
    let model = common::cases::case_model();

    let mut failures = Vec::new();
    let mut replayed = 0_usize;

    for case in common::cases::all_cases() {
        if !replayable(&case, &scenario_ids) {
            eprintln!("skipping {}: no recorded scenario for this case", case.id);
            continue;
        }

        // A fresh bearer namespace receives its own copy of the scenario
        // template, so one-shot scenario consumption stays per-case.
        let namespace = server.fork_namespace().expect("namespace should fork");
        match replay_case(&namespace.api_client(), &model, &case).await {
            Ok(turns) => {
                replayed += 1;
                for (snapshot_name, canonical) in turns {
                    if let Err(failure) = common::normalize::assert_named_snapshot(
                        &snapshot_name,
                        &canonical.canonical,
                    ) {
                        failures.push(failure);
                    }
                }
            }
            Err(error) => failures.push(format!("{}: replay failed: {error:#}", case.id)),
        }
    }

    assert!(
        replayed > 0,
        "no contract cases were replayed; the scenario fixtures look empty"
    );

    if failures.is_empty() {
        return;
    }

    panic!(
        "replay diverged from the recorded contract snapshots:\n{}",
        failures
            .iter()
            .map(|failure| format!("- {failure}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn scenario_ids(raw: &str) -> HashSet<String> {
    let envelope: Value = serde_json::from_str(raw).expect("scenario fixtures should parse");
    envelope
        .get("scenarios")
        .and_then(Value::as_array)
        .map(|scenarios| {
            scenarios
                .iter()
                .filter_map(|scenario| scenario.get("scenario_id"))
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn replayable(case: &ContractCase, scenario_ids: &HashSet<String>) -> bool {
    if !scenario_ids.contains(case.id) {
        return false;
    }
    if case.follow_up.is_some() && !scenario_ids.contains(&format!("{}__turn2", case.id)) {
        return false;
    }
    true
}

async fn replay_case(
    client: &common::ApiClient,
    model: &str,
    case: &ContractCase,
) -> Result<Vec<(String, CanonicalExchange)>> {
    let request = (case.build_request)(model);
    let mut turns = Vec::new();
    let first_body = replay_turn(client, case, case.id.to_owned(), &request, &mut turns).await?;

    if let Some(follow_up) = case.follow_up {
        let first_body = first_body.context("continuation cases need a non-stream first turn")?;
        let second_request = follow_up(model, &first_body);
        replay_turn(
            client,
            case,
            format!("{}__turn2", case.id),
            &second_request,
            &mut turns,
        )
        .await?;
    }

    Ok(turns)
}

async fn replay_turn(
    client: &common::ApiClient,
    case: &ContractCase,
    snapshot_name: String,
    request: &Value,
    turns: &mut Vec<(String, CanonicalExchange)>,
) -> Result<Option<Value>> {
    if case.stream {
        let exchange = common::post_sse_exchange(client, case.endpoint.path(), request).await?;
        let canonical = common::normalize::canonical_stream_exchange(
            case,
            exchange.status,
            exchange.content_type.as_deref(),
            &exchange.transcript,
        );
        turns.push((snapshot_name, canonical));
        Ok(None)
    } else {
        let exchange = common::post_json_exchange(client, case.endpoint.path(), request).await?;
        let canonical = common::normalize::canonical_json_exchange(
            case,
            exchange.status,
            exchange.content_type.as_deref(),
            &exchange.body,
        );
        turns.push((snapshot_name, canonical));
        Ok(Some(exchange.body))
    }
}
