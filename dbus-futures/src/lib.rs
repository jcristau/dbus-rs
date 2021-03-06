#![feature(futures_api)]

use dbus;
use std::sync::Arc;
use std::pin::Pin;
use futures::channel::{oneshot, mpsc};

use futures::task;
use futures::future::ready;

pub type Error = dbus::tree::MethodErr;

pub mod stdintf;

// To be sent to the backend
#[derive(Debug)]
enum Command {
    AddReply(u32, oneshot::Sender<dbus::Message>),
    Quit,
}

#[derive(Debug)]
pub struct ReplyMessage(Result<oneshot::Receiver<dbus::Message>, Option<Error>>);

impl futures::TryFuture for ReplyMessage {
    type Ok = dbus::Message;
    type Error = Error;
    fn try_poll(mut self: Pin<&mut Self>, lw: &task::LocalWaker) -> task::Poll<Result<Self::Ok, Self::Error>> {
        match &mut self.0 {
            Err(e) => task::Poll::Ready(Err(e.take().unwrap())),
            Ok(ref mut recv) => {
                use futures::Future;
                let p: Pin<&mut oneshot::Receiver<dbus::Message>> = Pin::new(recv);
                let mut r: dbus::Message = futures::try_ready!(p.poll(lw).map_err(|e| { Error::failed(&e) }));
                task::Poll::Ready((|| { r.as_result()?; Ok(r) })())
            }
        }
    }
}

impl ReplyMessage {
    pub fn new(serial: u32, handle: &ConnHandle) -> Self {
        let (s, r) = oneshot::channel();
        ReplyMessage(
            handle.1.unbounded_send(Command::AddReply(serial, s))
                .map_err(|e| { Some(Error::failed(&e)) })
                .map(|_| r)
        )
    }
}

pub struct MethodReply<T> {
    f: Pin<Box<futures::Future<Output=Result<T, Error>>>>,
}

impl<T> futures::TryFuture for MethodReply<T> {
    type Ok = T;
    type Error = Error;
    fn try_poll(mut self: Pin<&mut Self>, lw: &task::LocalWaker) -> task::Poll<Result<Self::Ok, Self::Error>> {
        let p = Pin::new(&mut self.f);
        p.try_poll(lw)
    }
}

impl<T: 'static> MethodReply<T> {
    pub fn from_msg<F: FnOnce(dbus::Message) -> Result<T, Error> + 'static>(msg: ReplyMessage, parse_fn: F) -> Self {
        use futures::TryFutureExt;
        MethodReply { f: Box::pin(msg.and_then(|m| ready(parse_fn(m))).into_future()) }
    }
}


#[derive(Clone, Debug)]
pub struct ConnPath<'a> {
    /// A clone of the connection handle
    pub conn: ConnHandle,
    /// Destination, i e what D-Bus service you're communicating with
    pub dest: dbus::BusName<'a>,
    /// Object path on the destination
    pub path: dbus::Path<'a>,
}

impl<'a> ConnPath<'a> {
    /// Make a D-Bus method call, where you can append arguments inside the closure.
    pub fn method_call_with_args<F>(&self, i: &dbus::Interface, m: &dbus::Member, f: F) -> ReplyMessage 
    where F: FnOnce(&mut dbus::Message)
    {
        let mut msg = dbus::Message::method_call(&self.dest, &self.path, i, m);
        f(&mut msg);
        match self.conn.send(msg) {
            Ok(serial) => ReplyMessage::new(serial, &self.conn),
            Err(e) => ReplyMessage(Err(Some(e))),
        }
    }

    /// Emit a D-Bus signal, where you can append arguments inside the closure.
    pub fn signal_with_args<F: FnOnce(&mut dbus::Message)>(&self, i: &dbus::Interface, m: &dbus::Member, f: F) -> Result<u32, Error> {
        let mut msg = dbus::Message::signal(&self.path, i, m);
        f(&mut msg);
        self.conn.send(msg)
    }

    /// Emit a D-Bus signal, where the arguments are in a struct.
    pub fn emit<S: dbus::SignalArgs>(&self, signal: &S) -> Result<u32, Error> {
        let msg = signal.to_emit_message(&self.path);
        self.conn.send(msg)
    }
}

#[derive(Clone, Debug)]
pub struct ConnHandle(Arc<dbus::TxRx>, mpsc::UnboundedSender<Command>);

impl ConnHandle {
    /// Get the connection's unique name.
    ///
    /// It's usually something like ":1.54"
    pub fn unique_name(&self) -> &str { self.0.unique_name().unwrap() }

    /// Puts a message into libdbus out queue.
    ///
    /// Returns a serial number than can be used to match against a reply.
    /// This does not flush the out queue, the messages are likely to be written the next time the main loop runs.
    pub fn send(&self, msg: dbus::Message) -> Result<u32, Error> {
        self.0.send(msg).map_err(|_| Error::from((dbus::ErrorName::from("org.freedesktop.DBus.Error.Failed"), "Sending message failed")))
    }

    /// Create a convenience struct for easier calling of many methods on the same destination and path.
    pub fn with_path<'a, D: Into<dbus::BusName<'a>>, P: Into<dbus::Path<'a>>>(&'a self, dest: D, path: P) -> ConnPath<'a> {
        ConnPath { conn: self.clone(), dest: dest.into(), path: path.into() }
    }

    /// Tells the TxRx part to quit from the event loop.
    pub fn quit(&self) -> Result<(), ()> {
         self.1.unbounded_send(Command::Quit).map_err(|_| ())
    }
}

#[cfg(feature = "tml")]
pub mod thin_main_loop;



