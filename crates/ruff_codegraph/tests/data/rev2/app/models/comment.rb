class Comment < ApplicationRecord
  belongs_to :ticket
  validates :body, presence: true

  after_commit :notify
  after_commit :notify

  def notify
    ticket.update(touched_at: Time.now)
  end
end
