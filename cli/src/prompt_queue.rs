use std::future::Future;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextPrompt {
    FromQueue(String),
    FromUser(String),
}

#[derive(Debug)]
pub struct PromptQueue {
    next_prompt: Option<String>,
}

impl PromptQueue {
    pub fn new(initial_prompt: String) -> Self {
        Self {
            next_prompt: Some(initial_prompt),
        }
    }

    pub fn pop_next_prompt<F>(&mut self, pop_queued_prompt: F) -> Option<String>
    where
        F: FnMut() -> Option<String>,
    {
        self.next_prompt.take().or_else(pop_queued_prompt)
    }
}

pub async fn next_prompt_or_prompt_user<F, Fut>(
    next_prompt: Option<String>,
    prompt_user: F,
) -> anyhow::Result<Option<NextPrompt>>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = anyhow::Result<Option<String>>>,
{
    match next_prompt {
        Some(prompt) => Ok(Some(NextPrompt::FromQueue(prompt))),
        None => {
            let Some(prompt) = prompt_user().await? else {
                return Ok(None);
            };
            Ok(Some(NextPrompt::FromUser(prompt)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use pretty_assertions::assert_eq;

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

    #[tokio::test]
    async fn next_prompt_or_prompt_user_uses_queue_first() {
        let mut queue = PromptQueue::new("initial".to_string());
        let next_prompt = queue.pop_next_prompt(|| None);
        let prompt = next_prompt_or_prompt_user(next_prompt, || async {
            anyhow::bail!("prompt_user should not run when queue has a prompt");
        })
        .await
        .expect("resolve prompt");
        assert_eq!(prompt, Some(NextPrompt::FromQueue("initial".to_string())));
    }

    #[tokio::test]
    async fn next_prompt_or_prompt_user_prompts_when_queue_empty() {
        let mut queue = PromptQueue::new("initial".to_string());
        assert_eq!(queue.pop_next_prompt(|| None), Some("initial".to_string()));

        let next_prompt = queue.pop_next_prompt(|| None);
        let prompt =
            next_prompt_or_prompt_user(next_prompt, || async { Ok(Some("fallback".to_string())) })
                .await
                .expect("resolve prompt");
        assert_eq!(prompt, Some(NextPrompt::FromUser("fallback".to_string())));
    }

    #[tokio::test]
    async fn next_prompt_or_prompt_user_propagates_user_cancel() {
        let mut queue = PromptQueue::new("initial".to_string());
        assert_eq!(queue.pop_next_prompt(|| None), Some("initial".to_string()));

        let next_prompt = queue.pop_next_prompt(|| None);
        let prompt = next_prompt_or_prompt_user(next_prompt, || async { Ok(None) })
            .await
            .expect("resolve prompt");
        assert_eq!(prompt, None);
    }
}
