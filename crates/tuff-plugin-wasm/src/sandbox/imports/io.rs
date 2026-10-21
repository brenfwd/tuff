use super::super::*;

impl State {
    pub(in crate::sandbox) fn background_run(
        &mut self,
        task: &str,
        input: &[u8],
    ) -> value::Result<Result<Handle, Denied>> {
        Ok(match &self.background {
            Some(background) => background.run(task, input),
            None => unavailable("background tasks"),
        })
    }

    pub(in crate::sandbox) fn background_cancel(&mut self, task: Handle) -> value::Result<()> {
        if let Some(background) = &self.background {
            background.cancel(task);
        }
        Ok(())
    }

    pub(in crate::sandbox) fn net_granted(
        &mut self,
        kind: CapabilityKind,
        target: &str,
    ) -> value::Result<bool> {
        Ok(self
            .net
            .as_ref()
            .is_some_and(|net| net.granted(kind, target)))
    }

    pub(in crate::sandbox) fn net_fetch(
        &mut self,
        request: HttpRequest,
    ) -> value::Result<Result<Handle, Denied>> {
        Ok(match &self.net {
            Some(net) => net.fetch(request),
            None => unavailable("http"),
        })
    }

    pub(in crate::sandbox) fn net_ws_connect(
        &mut self,
        url: &str,
        headers: Vec<Header>,
        protocols: Vec<String>,
    ) -> value::Result<Result<Handle, Denied>> {
        Ok(match &self.net {
            Some(net) => net.ws_connect(url, headers, protocols),
            None => unavailable("websocket"),
        })
    }

    pub(in crate::sandbox) fn net_ws_send(
        &mut self,
        socket: Handle,
        message: WsMessage,
    ) -> value::Result<Result<(), Denied>> {
        Ok(match &self.net {
            Some(net) => net.ws_send(socket, message),
            None => Err(Denied::Closed),
        })
    }

    pub(in crate::sandbox) fn net_tcp_connect(
        &mut self,
        host: &str,
        port: u16,
        tls: bool,
    ) -> value::Result<Result<Handle, Denied>> {
        Ok(match &self.net {
            Some(net) => net.tcp_connect(host, port, tls),
            None => unavailable("tcp"),
        })
    }

    pub(in crate::sandbox) fn net_udp_connect(
        &mut self,
        host: &str,
        port: u16,
    ) -> value::Result<Result<Handle, Denied>> {
        Ok(match &self.net {
            Some(net) => net.udp_connect(host, port),
            None => unavailable("udp"),
        })
    }

    pub(in crate::sandbox) fn net_send(
        &mut self,
        socket: Handle,
        data: &[u8],
    ) -> value::Result<Result<(), Denied>> {
        Ok(match &self.net {
            Some(net) => net.send(socket, data),
            None => Err(Denied::Closed),
        })
    }

    pub(in crate::sandbox) fn net_listen(
        &mut self,
        kind: CapabilityKind,
        bind: Option<String>,
        port: u16,
    ) -> value::Result<Result<Handle, Denied>> {
        Ok(match &self.net {
            Some(net) => net.listen(kind, bind.as_deref(), port),
            None => unavailable("listening"),
        })
    }

    pub(in crate::sandbox) fn net_reply_to(
        &mut self,
        listener: Handle,
        peer: &str,
        data: &[u8],
    ) -> value::Result<Result<(), Denied>> {
        Ok(match &self.net {
            Some(net) => net.reply_to(listener, peer, data),
            None => Err(Denied::Closed),
        })
    }

    pub(in crate::sandbox) fn net_respond(
        &mut self,
        request: Handle,
        response: RouteResponse,
    ) -> value::Result<Result<(), Denied>> {
        let body = match &response.body {
            crate::wit::bindings::capability_types::RouteBody::Bytes(body) => body.len(),
            crate::wit::bindings::capability_types::RouteBody::File(path) => path.len(),
        };
        let queued: usize = self
            .effects
            .responses
            .iter()
            .map(|(_, r)| match &r.body {
                crate::wit::bindings::capability_types::RouteBody::Bytes(body) => body.len(),
                crate::wit::bindings::capability_types::RouteBody::File(path) => path.len(),
            })
            .sum();
        if self.effects.responses.iter().any(|(h, _)| *h == request)
            || self.effects.responses.len() >= abi::MAX_ROUTE_RESPONSES
            || body > self.limits.message_bytes
            || queued + body > abi::MAX_RESPONSE_BYTES
        {
            return Ok(Err(Denied::Malformed(
                "route answer over the per-call limits or already given".into(),
            )));
        }
        self.effects.responses.push((request, response));
        Ok(Ok(()))
    }

    pub(in crate::sandbox) fn net_close(&mut self, target: Handle) -> value::Result<()> {
        if let Some(net) = &self.net {
            net.close(target);
        }
        Ok(())
    }

    pub(in crate::sandbox) fn files_read(
        &mut self,
        path: &str,
        offset: u64,
        len: u32,
    ) -> value::Result<Result<Handle, Denied>> {
        let op = validate(path, false).map(|path| Op::Read { path, offset, len });
        Ok(self.file_op(op))
    }

    pub(in crate::sandbox) fn files_write(
        &mut self,
        path: &str,
        mode: WriteMode,
        data: &[u8],
    ) -> value::Result<Result<Handle, Denied>> {
        let op = validate(path, false).map(|path| Op::Write {
            path,
            mode,
            data: data.to_vec(),
        });
        Ok(self.file_op(op))
    }

    pub(in crate::sandbox) fn files_list_dir(
        &mut self,
        path: &str,
    ) -> value::Result<Result<Handle, Denied>> {
        Ok(self.file_op(validate(path, true).map(Op::List)))
    }

    pub(in crate::sandbox) fn files_stat(
        &mut self,
        path: &str,
    ) -> value::Result<Result<Handle, Denied>> {
        Ok(self.file_op(validate(path, true).map(Op::Stat)))
    }

    pub(in crate::sandbox) fn files_remove(
        &mut self,
        path: &str,
    ) -> value::Result<Result<Handle, Denied>> {
        Ok(self.file_op(validate(path, false).map(Op::Remove)))
    }

    pub(in crate::sandbox) fn files_rename(
        &mut self,
        src: &str,
        dst: &str,
    ) -> value::Result<Result<Handle, Denied>> {
        let op = validate(src, false)
            .and_then(|src| validate(dst, false).map(|dst| Op::Rename(src, dst)));
        Ok(self.file_op(op))
    }

    pub(in crate::sandbox) fn files_create_dir(
        &mut self,
        path: &str,
    ) -> value::Result<Result<Handle, Denied>> {
        Ok(self.file_op(validate(path, false).map(Op::CreateDir)))
    }

    pub(in crate::sandbox) fn files_usage(&mut self) -> value::Result<(u64, u64)> {
        Ok(self.files.as_ref().map_or((0, 0), |files| files.usage()))
    }
}
