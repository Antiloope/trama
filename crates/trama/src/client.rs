// SPDX-License-Identifier: GPL-3.0-or-later

//! The command line as a client of the running server: it finds the
//! server through the user data folder and talks to its API.

use std::time::Duration;

use anyhow::{Context, bail};
use ureq::Agent;

use crate::api::{CONTROL_PATH, ControlRequest, ErrorReply, STATUS_PATH, Status};
use crate::user_data::{Running, UserData};

/// How long a client waits for the server to answer.
const TIMEOUT: Duration = Duration::from_secs(5);

/// The running server, as a client sees it.
pub struct Client {
    port: u16,
    agent: Agent,
}

impl Client {
    /// The server running on `user_data`, or an error that says how to
    /// start one.
    pub fn find(user_data: &UserData) -> anyhow::Result<Client> {
        let running = user_data
            .running_server()
            .context("reading the user data folder")?;
        match running {
            Some(Running::At { port }) => Ok(Client::at(port)),
            Some(Running::Starting) => bail!("Trama is starting; try again in a moment"),
            None => bail!("no Trama server is running; start one with `trama` in another terminal"),
        }
    }

    /// The server listening on `port` of localhost.
    pub fn at(port: u16) -> Client {
        let agent = Agent::config_builder()
            .timeout_global(Some(TIMEOUT))
            // Refusals carry a reason, which the client reads.
            .http_status_as_error(false)
            .build()
            .into();
        Client { port, agent }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn status(&self) -> anyhow::Result<Status> {
        let mut response = self
            .agent
            .get(self.url(STATUS_PATH))
            .call()
            .with_context(|| self.unreachable())?;
        if !response.status().is_success() {
            bail!(refusal(&mut response));
        }
        Ok(response.body_mut().read_json()?)
    }

    /// Sends a press, release or value to a control input of a module.
    pub fn control(&self, request: &ControlRequest) -> anyhow::Result<()> {
        let mut response = self
            .agent
            .post(self.url(CONTROL_PATH))
            .send_json(request)
            .with_context(|| self.unreachable())?;
        if !response.status().is_success() {
            bail!(refusal(&mut response));
        }
        Ok(())
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn unreachable(&self) -> String {
        format!("the Trama server on port {} does not answer", self.port)
    }
}

/// What a refusal says, or its status when it says nothing readable.
fn refusal(response: &mut ureq::http::Response<ureq::Body>) -> String {
    let status = response.status();
    match response.body_mut().read_json::<ErrorReply>() {
        Ok(reply) => reply.error,
        Err(_) => format!("the server answered {status}"),
    }
}
