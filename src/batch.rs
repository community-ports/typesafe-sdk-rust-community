//! Many states, one question set: concurrent requests with results aligned to inputs and one
//! shared backoff, plus the reranking and line-search cookbooks built on top.
//!
//! The API batches *questions* (one request, many questions) but not *states*: judging 500
//! tickets is 500 requests. [`Batch`] runs them with bounded concurrency, keeps every result in
//! input order as its own `Ok`/`Err`, and when any request is rate limited it pauses all of
//! them until the server's `Retry-After` elapses instead of letting eight tasks retry into the
//! same limit.
//!
//! ```no_run
//! use typesafeai_sdk_community::{NoulAnswer, Questions, TypeSafeClient};
//!
//! #[derive(Debug, Questions)]
//! struct Triage {
//!     #[noul("Is this ticket about billing?")]
//!     billing: NoulAnswer,
//! }
//!
//! # async fn run(client: TypeSafeClient, tickets: Vec<String>) -> typesafeai_sdk_community::Result<()> {
//! let outcome = client.batch::<Triage>(tickets).concurrency(8).run().await;
//! println!("{} ok, {} failed, {} input tokens", outcome.succeeded(), outcome.failed(), outcome.usage.input_tokens.unwrap_or(0));
//! for (index, triage) in outcome.ok() {
//!     println!("ticket {index}: billing={:.2}", triage.billing.noul);
//! }
//! # Ok(()) }
//! ```
//!
//! [`TypeSafeClient::rerank`] asks one relevance question per candidate and sorts;
//! [`TypeSafeClient::find`] puts every item in one request and asks which matches.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};

use crate::client::{TypeSafeClient, insert_header};
use crate::error::{Error, Result, parse_retry_after};
use crate::question::{Choice, Noul, Question, Score};
use crate::response::{Answer, ChoiceAnswer, NoulAnswer, SystemOneResponse, Usage};
use crate::retry::RetryPolicy;
use crate::transport::{HttpRequest, HttpResponse, Transport};
use crate::typed::{Answered, Questions};

// --- Shared pacing ------------------------------------------------------------------------------

/// A shared "do not send before" instant, set by rate-limit responses and honored by every
/// request going through a [`PacedTransport`].
#[derive(Clone, Default)]
pub struct Pacer {
    until: Arc<Mutex<Option<Instant>>>,
}

impl Pacer {
    /// A pacer with no pause.
    pub fn new() -> Self {
        Self::default()
    }

    /// Pause all requests until at least `delay` from now (never shortens an existing pause).
    pub fn pause_for(&self, delay: Duration) {
        let until = Instant::now() + delay;
        let mut current = self.until.lock().expect("pacer lock");
        if current.is_none_or(|existing| until > existing) {
            *current = Some(until);
        }
    }

    /// How long until requests may be sent, or zero.
    pub fn remaining(&self) -> Duration {
        self.until
            .lock()
            .expect("pacer lock")
            .map_or(Duration::ZERO, |until| until.saturating_duration_since(Instant::now()))
    }

    async fn wait(&self) {
        let remaining = self.remaining();
        if !remaining.is_zero() {
            tracing::info!(target: crate::logging::TARGET, "batch paused {}ms for a shared rate limit", remaining.as_millis());
            tokio::time::sleep(remaining).await;
        }
    }

    /// Record a rate-limit response: honor `Retry-After` / `retry-after-ms`, or a short default.
    fn observe(&self, response: &HttpResponse) {
        if matches!(response.status.as_u16(), 429 | 503) {
            self.pause_for(parse_retry_after(&response.headers).unwrap_or(Duration::from_secs(1)));
        }
    }
}

/// A [`Transport`] that waits for a shared [`Pacer`] before each attempt and feeds rate-limit
/// responses back into it. Wrap any transport with it to make independent tasks back off
/// together.
pub struct PacedTransport {
    inner: Arc<dyn Transport>,
    pacer: Pacer,
}

impl PacedTransport {
    /// Pace `inner` with `pacer`.
    pub fn new(inner: Arc<dyn Transport>, pacer: Pacer) -> Self {
        PacedTransport { inner, pacer }
    }
}

impl Transport for PacedTransport {
    fn send(&self, request: HttpRequest) -> Pin<Box<dyn Future<Output = Result<HttpResponse>> + Send + '_>> {
        Box::pin(async move {
            self.pacer.wait().await;
            let response = self.inner.send(request).await?;
            self.pacer.observe(&response);
            Ok(response)
        })
    }

    fn reqwest_client(&self) -> Option<&reqwest::Client> {
        self.inner.reqwest_client()
    }
}

// --- Batch --------------------------------------------------------------------------------------

/// Progress after each completed item, for [`Batch::on_progress`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// The input index that just finished.
    pub index: usize,
    /// Whether it succeeded.
    pub ok: bool,
    /// Items finished so far, including this one.
    pub completed: usize,
    /// Total items in the batch.
    pub total: usize,
}

type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

/// A batch under construction; see [`TypeSafeClient::batch`].
#[must_use = "a batch does nothing until it is run"]
pub struct Batch<'a, T> {
    client: &'a TypeSafeClient,
    states: Vec<Value>,
    concurrency: usize,
    model: Option<String>,
    retry: Option<RetryPolicy>,
    timeout: Option<Duration>,
    headers: HeaderMap,
    extra: BTreeMap<String, Question>,
    progress: Option<ProgressFn>,
    pacer: Option<Pacer>,
    header_error: Option<Error>,
    _answers: std::marker::PhantomData<fn() -> T>,
}

impl<'a, T: Questions> Batch<'a, T> {
    pub(crate) fn new(client: &'a TypeSafeClient, states: impl IntoIterator<Item = impl Into<Value>>) -> Self {
        Batch {
            client,
            states: states.into_iter().map(Into::into).collect(),
            concurrency: 4,
            model: None,
            retry: None,
            timeout: None,
            headers: HeaderMap::new(),
            extra: BTreeMap::new(),
            progress: None,
            pacer: None,
            header_error: None,
            _answers: std::marker::PhantomData,
        }
    }

    /// Maximum requests in flight at once. Default 4.
    pub fn concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }

    /// Model override for every request in the batch.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// A retry policy for every request in the batch.
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = Some(retry);
        self
    }

    /// An HTTP timeout for every request in the batch.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// An additional header on every request in the batch.
    pub fn header<N, V>(mut self, name: N, value: V) -> Self
    where
        N: TryInto<HeaderName>,
        N::Error: fmt::Display,
        V: TryInto<HeaderValue>,
        V::Error: fmt::Display,
    {
        insert_header(&mut self.headers, &mut self.header_error, name, value);
        self
    }

    /// An ad-hoc question added to every request, available on each item's response.
    pub fn question(mut self, name: impl Into<String>, question: impl Into<Question>) -> Self {
        self.extra.insert(name.into(), question.into());
        self
    }

    /// Called after each item completes, from whichever task finished it.
    pub fn on_progress(mut self, f: impl Fn(Progress) + Send + Sync + 'static) -> Self {
        self.progress = Some(Arc::new(f));
        self
    }

    /// Share a [`Pacer`] with other batches or clients so they back off together. By default
    /// each batch has its own.
    pub fn pacer(mut self, pacer: Pacer) -> Self {
        self.pacer = Some(pacer);
        self
    }

    /// Run every request and collect the outcome. Never fails as a whole: each item is its own
    /// `Result`, in input order.
    pub async fn run(self) -> BatchOutcome<Answered<T>> {
        let started = Instant::now();
        let total = self.states.len();
        if let Some(error) = self.header_error {
            return BatchOutcome::failed_setup(error, total, started.elapsed());
        }
        let pacer = self.pacer.unwrap_or_default();
        let client = self.client.paced(pacer);
        let (model, retry, timeout, headers, extra, progress) =
            (self.model, self.retry, self.timeout, self.headers, self.extra, self.progress);
        let completed = std::sync::atomic::AtomicUsize::new(0);

        let mut slots: Vec<Option<Result<Answered<T>>>> = (0..total).map(|_| None).collect();
        let mut stream = futures_util::stream::iter(self.states.into_iter().enumerate())
            .map(|(index, state)| {
                let (client, model, retry, timeout, headers, extra) =
                    (&client, &model, &retry, &timeout, &headers, &extra);
                async move {
                    let mut request = client.ask::<T>(state);
                    if let Some(model) = model {
                        request = request.model(model.clone());
                    }
                    if let Some(retry) = retry {
                        request = request.retry(retry.clone());
                    }
                    if let Some(timeout) = timeout {
                        request = request.timeout(*timeout);
                    }
                    for (name, value) in headers {
                        request = request.header(name.clone(), value.clone());
                    }
                    for (name, question) in extra {
                        request = request.question(name.clone(), question.clone());
                    }
                    (index, request.send_full().await)
                }
            })
            .buffer_unordered(self.concurrency);

        while let Some((index, result)) = stream.next().await {
            let done = completed.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            if let Some(progress) = &progress {
                progress(Progress { index, ok: result.is_ok(), completed: done, total });
            }
            slots[index] = Some(result);
        }
        drop(stream);

        let results: Vec<Result<Answered<T>>> = slots
            .into_iter()
            .map(|slot| slot.unwrap_or_else(|| Err(Error::InvalidRequest("batch item did not complete".into()))))
            .collect();
        BatchOutcome::collect(results, started.elapsed(), |answered| &answered.response)
    }
}

/// The result of a batch: one `Result` per input, in input order, plus totals.
#[derive(Debug)]
pub struct BatchOutcome<T> {
    /// One result per input state, in the same order.
    pub results: Vec<Result<T>>,
    /// Token usage summed over the successful requests.
    pub usage: Usage,
    /// Wall-clock time for the whole batch.
    pub elapsed: Duration,
}

impl<T> BatchOutcome<T> {
    fn collect(results: Vec<Result<T>>, elapsed: Duration, response: impl Fn(&T) -> &SystemOneResponse) -> Self {
        let mut usage = Usage::default();
        for item in results.iter().flatten() {
            let item_usage = &response(item).usage;
            usage.input_tokens = Some(usage.input_tokens.unwrap_or(0) + item_usage.input_tokens.unwrap_or(0));
            usage.output_tokens = Some(usage.output_tokens.unwrap_or(0) + item_usage.output_tokens.unwrap_or(0));
        }
        BatchOutcome { results, usage, elapsed }
    }

    fn failed_setup(error: Error, total: usize, elapsed: Duration) -> Self {
        let message = error.to_string();
        let results = (0..total).map(|_| Err(Error::Config(message.clone()))).collect();
        BatchOutcome { results, usage: Usage::default(), elapsed }
    }

    /// How many items succeeded.
    pub fn succeeded(&self) -> usize {
        self.results.iter().filter(|r| r.is_ok()).count()
    }

    /// How many items failed.
    pub fn failed(&self) -> usize {
        self.results.len() - self.succeeded()
    }

    /// Successful items with their input index.
    pub fn ok(&self) -> impl Iterator<Item = (usize, &T)> {
        self.results.iter().enumerate().filter_map(|(i, r)| r.as_ref().ok().map(|v| (i, v)))
    }

    /// Failed items with their input index.
    pub fn errors(&self) -> impl Iterator<Item = (usize, &Error)> {
        self.results.iter().enumerate().filter_map(|(i, r)| r.as_ref().err().map(|e| (i, e)))
    }

    /// All results, or the first error if any item failed.
    pub fn into_all(self) -> Result<Vec<T>> {
        self.results.into_iter().collect()
    }

    /// Map every successful item.
    pub fn map<U>(self, f: impl Fn(T) -> U) -> BatchOutcome<U> {
        BatchOutcome {
            results: self.results.into_iter().map(|r| r.map(&f)).collect(),
            usage: self.usage,
            elapsed: self.elapsed,
        }
    }
}

// --- Rerank -------------------------------------------------------------------------------------

/// How a candidate's relevance is judged.
#[derive(Clone, Debug)]
enum Relevance {
    /// A yes/no question; relevance is the probability of yes.
    Noul { instructions: Value, criteria: Option<(Value, Value)> },
    /// A graded rubric; relevance is the expected score over the top level.
    Score { instructions: Value, levels: Vec<Value> },
}

/// A candidate with its relevance.
#[derive(Clone, Debug)]
pub struct Ranked<C> {
    /// The candidate's position in the input.
    pub index: usize,
    /// The candidate.
    pub candidate: C,
    /// Relevance from 0 to 1.
    pub relevance: f64,
    /// The underlying answer: a noul or a score.
    pub answer: Answer,
}

/// Reranking as a typed question set: one request per candidate.
struct RelevanceQuestions;

/// A rerank under construction; see [`TypeSafeClient::rerank`].
#[must_use = "a rerank does nothing until it is run"]
pub struct Rerank<'a, C> {
    client: &'a TypeSafeClient,
    query: Value,
    candidates: Vec<C>,
    relevance: Relevance,
    concurrency: usize,
    top: Option<usize>,
    model: Option<String>,
    retry: Option<RetryPolicy>,
    pacer: Option<Pacer>,
}

impl<'a, C: Clone + Into<Value>> Rerank<'a, C> {
    pub(crate) fn new(client: &'a TypeSafeClient, query: impl Into<Value>, candidates: Vec<C>) -> Self {
        Rerank {
            client,
            query: query.into(),
            candidates,
            relevance: Relevance::Noul {
                instructions: json!("Does `candidate` contain information that answers or directly addresses `query`?"),
                criteria: Some((
                    json!(
                        "The candidate answers the query or is the kind of passage someone asking it is looking for."
                    ),
                    json!("The candidate is off topic, or only shares words with the query without addressing it."),
                )),
            },
            concurrency: 4,
            top: None,
            model: None,
            retry: None,
            pacer: None,
        }
    }

    /// Replace the yes/no relevance question. The state has `query` and `candidate` fields.
    pub fn instructions(mut self, instructions: impl Into<Value>) -> Self {
        let value = instructions.into();
        self.relevance = match self.relevance {
            Relevance::Noul { criteria, .. } => Relevance::Noul { instructions: value, criteria },
            Relevance::Score { levels, .. } => Relevance::Score { instructions: value, levels },
        };
        self
    }

    /// Judge relevance on an ordered rubric instead of yes/no; relevance is the expected score
    /// scaled to 0..1.
    pub fn graded(
        mut self,
        instructions: impl Into<Value>,
        levels: impl IntoIterator<Item = impl Into<Value>>,
    ) -> Self {
        self.relevance = Relevance::Score {
            instructions: instructions.into(),
            levels: levels.into_iter().map(Into::into).collect(),
        };
        self
    }

    /// Keep only the `n` most relevant candidates.
    pub fn top(mut self, n: usize) -> Self {
        self.top = Some(n);
        self
    }

    /// Maximum requests in flight at once. Default 4.
    pub fn concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }

    /// Model override for every request.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// A retry policy for every request.
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = Some(retry);
        self
    }

    /// Share a [`Pacer`] with other work.
    pub fn pacer(mut self, pacer: Pacer) -> Self {
        self.pacer = Some(pacer);
        self
    }

    /// Run the rerank. Fails if any candidate's request fails; use [`run_lenient`](Self::run_lenient)
    /// to drop failures instead.
    pub async fn run(self) -> Result<Vec<Ranked<C>>> {
        let (ranked, errors) = self.run_inner().await;
        if let Some((_, error)) = errors.into_iter().next() {
            return Err(error);
        }
        Ok(ranked)
    }

    /// Run the rerank, dropping candidates whose request failed and returning those errors.
    pub async fn run_lenient(self) -> (Vec<Ranked<C>>, Vec<(usize, Error)>) {
        self.run_inner().await
    }

    async fn run_inner(self) -> (Vec<Ranked<C>>, Vec<(usize, Error)>) {
        let question = match &self.relevance {
            Relevance::Noul { instructions, criteria } => {
                let mut noul = Noul { instructions: Some(instructions.clone()), criteria: None };
                if let Some((yes, no)) = criteria {
                    noul = noul.when_true(yes.clone()).when_false(no.clone());
                }
                Question::Noul(noul)
            }
            Relevance::Score { instructions, levels } => {
                Question::Score(Score { instructions: Some(instructions.clone()), criteria: levels.clone() })
            }
        };
        let states = self
            .candidates
            .iter()
            .cloned()
            .map(|candidate| json!({"query": self.query, "candidate": candidate.into()}));
        let mut batch = self
            .client
            .batch::<RelevanceQuestions>(states)
            .concurrency(self.concurrency)
            .question("relevance", question);
        if let Some(model) = self.model {
            batch = batch.model(model);
        }
        if let Some(retry) = self.retry {
            batch = batch.retry(retry);
        }
        if let Some(pacer) = self.pacer {
            batch = batch.pacer(pacer);
        }
        let outcome = batch.run().await;

        let mut ranked = Vec::new();
        let mut errors = Vec::new();
        for (index, (candidate, result)) in self.candidates.into_iter().zip(outcome.results).enumerate() {
            match result {
                Ok(answered) => match answered.response.answer("relevance") {
                    Some(answer) => {
                        let relevance = match answer {
                            Answer::Noul(noul) => noul.noul,
                            Answer::Score(score) => score.normalized(),
                            _ => 0.0,
                        };
                        ranked.push(Ranked { index, candidate, relevance, answer: answer.clone() });
                    }
                    None => errors
                        .push((index, Error::Answer(crate::error::AnswerError::Missing { name: "relevance".into() }))),
                },
                Err(error) => errors.push((index, error)),
            }
        }
        ranked.sort_by(|a, b| b.relevance.total_cmp(&a.relevance).then(a.index.cmp(&b.index)));
        if let Some(n) = self.top {
            ranked.truncate(n);
        }
        (ranked, errors)
    }
}

impl Questions for RelevanceQuestions {
    fn questions() -> BTreeMap<String, Question> {
        BTreeMap::new() // the relevance question is added per batch, so it can be customized
    }
    fn from_response(_: &SystemOneResponse) -> std::result::Result<Self, crate::error::AnswerError> {
        Ok(RelevanceQuestions)
    }
}

// --- Find ---------------------------------------------------------------------------------------

/// The result of [`TypeSafeClient::find`]: which items match a query, from one request.
#[derive(Clone, Debug)]
pub struct Found {
    /// Whether any item answers the query, as a probability.
    pub present: NoulAnswer,
    /// Item indices ranked by the probability that they are the best match.
    pub ranked: Vec<(usize, f64)>,
    /// The routing choice over item indices.
    pub choice: ChoiceAnswer,
    /// The full response.
    pub response: SystemOneResponse,
}

impl Found {
    /// The most likely matching item, if the model thinks any item matches at `threshold`.
    pub fn best(&self, threshold: f64) -> Option<usize> {
        if self.present.noul >= threshold { self.ranked.first().map(|(index, _)| *index) } else { None }
    }

    /// The `n` most likely items.
    pub fn top(&self, n: usize) -> &[(usize, f64)] {
        &self.ranked[..n.min(self.ranked.len())]
    }
}

/// A find under construction; see [`TypeSafeClient::find`].
#[must_use = "a find does nothing until it is run"]
pub struct Find<'a> {
    client: &'a TypeSafeClient,
    query: Value,
    items: Vec<Value>,
    which: Value,
    any: Value,
    model: Option<String>,
    retry: Option<RetryPolicy>,
}

impl<'a> Find<'a> {
    pub(crate) fn new(
        client: &'a TypeSafeClient,
        query: impl Into<Value>,
        items: impl IntoIterator<Item = impl Into<Value>>,
    ) -> Self {
        Find {
            client,
            query: query.into(),
            items: items.into_iter().map(Into::into).collect(),
            which: json!("Which entry in `items` best answers or matches `query`? Choose its `id`."),
            any: json!("Does at least one entry in `items` answer or directly address `query`?"),
            model: None,
            retry: None,
        }
    }

    /// Replace the "which item" instructions. The state has `query` and `items` (each with `id`
    /// and `text`).
    pub fn instructions(mut self, which: impl Into<Value>) -> Self {
        self.which = which.into();
        self
    }

    /// Replace the "does any item match" instructions.
    pub fn presence_instructions(mut self, any: impl Into<Value>) -> Self {
        self.any = any.into();
        self
    }

    /// Model override.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// A retry policy for this request.
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = Some(retry);
        self
    }

    /// Run the single request.
    pub async fn run(self) -> Result<Found> {
        if self.items.is_empty() {
            return Err(Error::InvalidRequest("find needs at least one item".into()));
        }
        let ids: Vec<String> = (0..self.items.len()).map(|i| i.to_string()).collect();
        let state = json!({
            "query": self.query,
            "items": self.items.iter().zip(&ids).map(|(text, id)| json!({"id": id, "text": text})).collect::<Vec<_>>(),
        });
        let mut request = self
            .client
            .system_one(state)
            .question(
                "which",
                Choice { instructions: Some(self.which), criteria: ids.iter().map(|id| (id.clone(), None)).collect() },
            )
            .question("any", Noul { instructions: Some(self.any), criteria: None });
        if let Some(model) = self.model {
            request = request.model(model);
        }
        if let Some(retry) = self.retry {
            request = request.retry(retry);
        }
        let response = request.send().await?;
        let choice = response
            .choice("which")
            .cloned()
            .ok_or_else(|| crate::error::AnswerError::Missing { name: "which".into() })?;
        let present =
            response.noul("any").cloned().ok_or_else(|| crate::error::AnswerError::Missing { name: "any".into() })?;
        let mut ranked: Vec<(usize, f64)> =
            choice.probabilities.iter().filter_map(|(id, p)| Some((id.parse::<usize>().ok()?, *p))).collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        Ok(Found { present, ranked, choice, response })
    }
}

// --- Client entry points ------------------------------------------------------------------------

impl TypeSafeClient {
    /// Ask a question set about many states concurrently; see the [`batch`](crate::batch) module.
    pub fn batch<T: Questions>(&self, states: impl IntoIterator<Item = impl Into<Value>>) -> Batch<'_, T> {
        Batch::new(self, states)
    }

    /// Rank candidates by relevance to a query, one request per candidate, following the
    /// [reranking cookbook](https://docs.typesafe.ai/cookbooks/rerank_typesafe).
    ///
    /// ```no_run
    /// # use typesafeai_sdk_community::TypeSafeClient;
    /// # async fn run(client: TypeSafeClient, passages: Vec<String>) -> typesafeai_sdk_community::Result<()> {
    /// let ranked = client.rerank("How do I get a refund?", passages).top(5).concurrency(8).run().await?;
    /// for hit in &ranked {
    ///     println!("{:.2} [{}] {}", hit.relevance, hit.index, hit.candidate);
    /// }
    /// # Ok(()) }
    /// ```
    pub fn rerank<C: Clone + Into<Value>>(&self, query: impl Into<Value>, candidates: Vec<C>) -> Rerank<'_, C> {
        Rerank::new(self, query, candidates)
    }

    /// Find which of many small items (lines, sentences, rows) answers a query, in one request,
    /// following the [line-by-line search cookbook](https://docs.typesafe.ai/cookbooks/semantic_find).
    /// Prefer this over [`rerank`](Self::rerank) when the items fit comfortably in one request.
    ///
    /// ```no_run
    /// # use typesafeai_sdk_community::TypeSafeClient;
    /// # async fn run(client: TypeSafeClient, lines: Vec<String>) -> typesafeai_sdk_community::Result<()> {
    /// let found = client.find("When can I cancel?", lines.clone()).run().await?;
    /// if let Some(index) = found.best(0.5) {
    ///     println!("line {index}: {}", lines[index]);
    /// }
    /// # Ok(()) }
    /// ```
    pub fn find(&self, query: impl Into<Value>, items: impl IntoIterator<Item = impl Into<Value>>) -> Find<'_> {
        Find::new(self, query, items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;
    use reqwest::header::HeaderValue;

    #[test]
    fn pacer_never_shortens() {
        let pacer = Pacer::new();
        assert_eq!(pacer.remaining(), Duration::ZERO);
        pacer.pause_for(Duration::from_millis(200));
        pacer.pause_for(Duration::from_millis(50));
        assert!(pacer.remaining() > Duration::from_millis(100));
    }

    #[test]
    fn pacer_observes_rate_limits() {
        let pacer = Pacer::new();
        let mut headers = HeaderMap::new();
        headers.insert("retry-after-ms", HeaderValue::from_static("300"));
        pacer.observe(&HttpResponse { status: StatusCode::TOO_MANY_REQUESTS, headers, body: Vec::new() });
        assert!(pacer.remaining() > Duration::from_millis(200));
        let other = Pacer::new();
        other.observe(&HttpResponse { status: StatusCode::OK, headers: HeaderMap::new(), body: Vec::new() });
        assert_eq!(other.remaining(), Duration::ZERO);
        let default = Pacer::new();
        default.observe(&HttpResponse {
            status: StatusCode::SERVICE_UNAVAILABLE,
            headers: HeaderMap::new(),
            body: Vec::new(),
        });
        assert!(default.remaining() > Duration::from_millis(900));
    }
}
