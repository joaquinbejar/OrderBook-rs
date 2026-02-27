/******************************************************************************
   Author: Joaquín Béjar García
   Email: jb@taunais.com
   Date: 2/10/25
******************************************************************************/

//! Multi-book management with centralized trade event routing.
//!
//! This module provides book management through a trait-based design, with implementations
//! for both standard library (`BookManagerStd`) and Tokio (`BookManagerTokio`) channels.

use crate::orderbook::OrderBook;
use crate::orderbook::trade::{TradeEvent, TradeListener, TradeResult};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{error, info};

/// Trait for managing multiple order books with centralized trade event routing.
///
/// This trait defines the interface for book managers, allowing different
/// implementations using various channel types (std::mpsc, tokio::mpsc, etc.).
pub trait BookManager<T>
where
    T: Clone + Send + Sync + Default + 'static,
{
    /// Add a new order book for a symbol with an automatically configured trade listener.
    fn add_book(&mut self, symbol: &str);

    /// Get a reference to an order book by symbol.
    fn get_book(&self, symbol: &str) -> Option<&OrderBook<T>>;

    /// Get a mutable reference to an order book by symbol.
    fn get_book_mut(&mut self, symbol: &str) -> Option<&mut OrderBook<T>>;

    /// Get the list of all symbols with order books in this manager.
    fn symbols(&self) -> Vec<String>;

    /// Remove an order book for a specific symbol.
    fn remove_book(&mut self, symbol: &str) -> Option<OrderBook<T>>;

    /// Check if a book exists for a specific symbol.
    fn has_book(&self, symbol: &str) -> bool;

    /// Get the number of order books in this manager.
    fn book_count(&self) -> usize;
}

/// BookManager implementation using standard library mpsc channels.
pub struct BookManagerStd<T>
where
    T: Clone + Send + Sync + Default + 'static,
{
    /// Collection of order books indexed by symbol
    books: HashMap<String, OrderBook<T>>,
    /// Sender for trade events
    trade_sender: std::sync::mpsc::Sender<TradeEvent>,
    /// Receiver for trade events (taken when processor starts)
    trade_receiver: Option<std::sync::mpsc::Receiver<TradeEvent>>,
}

impl<T> BookManagerStd<T>
where
    T: Clone + Send + Sync + Default + 'static,
{
    /// Create a new BookManagerStd with a standard library mpsc channel.
    pub fn new() -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();

        Self {
            books: HashMap::new(),
            trade_sender: sender,
            trade_receiver: Some(receiver),
        }
    }

    /// Start the trade event processor in a separate thread.
    pub fn start_trade_processor(&mut self) -> std::thread::JoinHandle<()> {
        let receiver = self
            .trade_receiver
            .take()
            .expect("Trade processor already started");

        std::thread::spawn(move || {
            info!("Trade processor started");

            while let Ok(trade_event) = receiver.recv() {
                Self::process_trade_event(trade_event);
            }

            info!("Trade processor stopped");
        })
    }

    /// Process a single trade event.
    fn process_trade_event(event: TradeEvent) {
        info!(
            "Processing trade for {}: {} trades, executed quantity: {}",
            event.symbol,
            event.trade_result.match_result.trades().as_vec().len(),
            event
                .trade_result
                .match_result
                .executed_quantity()
                .unwrap_or(0)
        );

        for trade in event.trade_result.match_result.trades().as_vec() {
            info!(
                "  Trade: {} units at price {} (ID: {})",
                trade.quantity(),
                trade.price(),
                trade.trade_id()
            );
        }
    }
}

impl<T> BookManager<T> for BookManagerStd<T>
where
    T: Clone + Send + Sync + Default + 'static,
{
    fn add_book(&mut self, symbol: &str) {
        let sender = self.trade_sender.clone();
        let symbol_clone = symbol.to_string();

        let trade_listener: TradeListener = Arc::new(move |trade_result: &TradeResult| {
            let trade_event = TradeEvent {
                symbol: trade_result.symbol.clone(),
                trade_result: trade_result.clone(),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64,
            };

            if let Err(e) = sender.send(trade_event) {
                error!("Failed to send trade event for {}: {}", symbol_clone, e);
            }
        });

        let book = OrderBook::with_trade_listener(symbol, trade_listener);
        self.books.insert(symbol.to_string(), book);
        info!("Added order book for symbol: {}", symbol);
    }

    fn get_book(&self, symbol: &str) -> Option<&OrderBook<T>> {
        self.books.get(symbol)
    }

    fn get_book_mut(&mut self, symbol: &str) -> Option<&mut OrderBook<T>> {
        self.books.get_mut(symbol)
    }

    fn symbols(&self) -> Vec<String> {
        self.books.keys().cloned().collect()
    }

    fn remove_book(&mut self, symbol: &str) -> Option<OrderBook<T>> {
        let result = self.books.remove(symbol);
        if result.is_some() {
            info!("Removed order book for symbol: {}", symbol);
        }
        result
    }

    fn has_book(&self, symbol: &str) -> bool {
        self.books.contains_key(symbol)
    }

    fn book_count(&self) -> usize {
        self.books.len()
    }
}

impl<T> Default for BookManagerStd<T>
where
    T: Clone + Send + Sync + Default + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

/// BookManager implementation using Tokio mpsc channels.
pub struct BookManagerTokio<T>
where
    T: Clone + Send + Sync + Default + 'static,
{
    /// Collection of order books indexed by symbol
    books: HashMap<String, OrderBook<T>>,
    /// Sender for trade events
    trade_sender: tokio::sync::mpsc::UnboundedSender<TradeEvent>,
    /// Receiver for trade events (taken when processor starts)
    trade_receiver: Option<tokio::sync::mpsc::UnboundedReceiver<TradeEvent>>,
}

impl<T> BookManagerTokio<T>
where
    T: Clone + Send + Sync + Default + 'static,
{
    /// Create a new BookManagerTokio with a Tokio unbounded mpsc channel.
    pub fn new() -> Self {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();

        Self {
            books: HashMap::new(),
            trade_sender: sender,
            trade_receiver: Some(receiver),
        }
    }

    /// Start the trade event processor as an async task.
    ///
    /// Returns a JoinHandle for the spawned task.
    pub fn start_trade_processor(&mut self) -> tokio::task::JoinHandle<()> {
        let mut receiver = self
            .trade_receiver
            .take()
            .expect("Trade processor already started");

        tokio::spawn(async move {
            info!("Trade processor started (Tokio)");

            while let Some(trade_event) = receiver.recv().await {
                Self::process_trade_event(trade_event);
            }

            info!("Trade processor stopped (Tokio)");
        })
    }

    /// Process a single trade event.
    fn process_trade_event(event: TradeEvent) {
        info!(
            "Processing trade for {}: {} trades, executed quantity: {}",
            event.symbol,
            event.trade_result.match_result.trades().as_vec().len(),
            event
                .trade_result
                .match_result
                .executed_quantity()
                .unwrap_or(0)
        );

        for trade in event.trade_result.match_result.trades().as_vec() {
            info!(
                "  Trade: {} units at price {} (ID: {})",
                trade.quantity(),
                trade.price(),
                trade.trade_id()
            );
        }
    }
}

impl<T> BookManager<T> for BookManagerTokio<T>
where
    T: Clone + Send + Sync + Default + 'static,
{
    fn add_book(&mut self, symbol: &str) {
        let sender = self.trade_sender.clone();
        let symbol_clone = symbol.to_string();

        let trade_listener: TradeListener = Arc::new(move |trade_result: &TradeResult| {
            let trade_event = TradeEvent {
                symbol: trade_result.symbol.clone(),
                trade_result: trade_result.clone(),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64,
            };

            if let Err(e) = sender.send(trade_event) {
                error!("Failed to send trade event for {}: {}", symbol_clone, e);
            }
        });

        let book = OrderBook::with_trade_listener(symbol, trade_listener);
        self.books.insert(symbol.to_string(), book);
        info!("Added order book for symbol: {}", symbol);
    }

    fn get_book(&self, symbol: &str) -> Option<&OrderBook<T>> {
        self.books.get(symbol)
    }

    fn get_book_mut(&mut self, symbol: &str) -> Option<&mut OrderBook<T>> {
        self.books.get_mut(symbol)
    }

    fn symbols(&self) -> Vec<String> {
        self.books.keys().cloned().collect()
    }

    fn remove_book(&mut self, symbol: &str) -> Option<OrderBook<T>> {
        let result = self.books.remove(symbol);
        if result.is_some() {
            info!("Removed order book for symbol: {}", symbol);
        }
        result
    }

    fn has_book(&self, symbol: &str) -> bool {
        self.books.contains_key(symbol)
    }

    fn book_count(&self) -> usize {
        self.books.len()
    }
}

impl<T> Default for BookManagerTokio<T>
where
    T: Clone + Send + Sync + Default + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}
