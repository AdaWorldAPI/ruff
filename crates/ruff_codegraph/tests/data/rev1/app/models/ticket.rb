class Ticket < ApplicationRecord
  include Trackable

  belongs_to :customer, class_name: 'Contact', optional: true
  has_many :comments, dependent: :destroy

  validates :title, presence: true
  validates :title, length: { maximum: 120 }
  validate :check_state
  validates_with StateValidator

  before_save :normalize_title
  before_save :stamp_owner
  after_create AuditCallback.new
  after_commit :announce, if: :open?

  scope :open_tickets, -> { where(state: 'open') }
  delegate :name, to: :customer, prefix: true

  def close
    self.state = 'closed'
    self.touch
    self.save!
  end

  def touch
    self.touched_at = Time.now
  end

  def summary
    title + title
  end

  private

  def normalize_title
    self.title = title.strip
  end

  def stamp_owner
    self.owner ||= 'system'
  end

  def check_state
    raise StateError unless %w[open closed].include?(state)
  end
end
