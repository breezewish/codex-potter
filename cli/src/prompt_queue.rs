#[derive(Debug)]
pub(crate) struct PromptQueue {
    next_prompt: Option<String>,
}

impl PromptQueue {
    pub(crate) fn new(initial_prompt: String) -> Self {
        Self {
            next_prompt: Some(initial_prompt),
        }
    }

    pub(crate) fn pop_next_prompt<F>(&mut self, pop_queued_prompt: F) -> Option<String>
    where
        F: FnMut() -> Option<String>,
    {
        self.next_prompt.take().or_else(pop_queued_prompt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pop_next_prompt_drains_initial_then_queued_in_order() {
        let mut queue = PromptQueue::new("initial".to_string());
        let mut queued = std::collections::VecDeque::from(["one".to_string(), "two".to_string()]);

        assert_eq!(
            queue.pop_next_prompt(|| queued.pop_front()),
            Some("initial".to_string())
        );
        assert_eq!(
            queue.pop_next_prompt(|| queued.pop_front()),
            Some("one".to_string())
        );
        assert_eq!(
            queue.pop_next_prompt(|| queued.pop_front()),
            Some("two".to_string())
        );
        assert_eq!(queue.pop_next_prompt(|| queued.pop_front()), None);
        assert_eq!(queued, std::collections::VecDeque::<String>::new());
    }
}
